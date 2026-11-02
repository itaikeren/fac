//! bash and fish started the way fac starts them: their launch spec, their
//! hook script and the command pipe, under a scratch HOME, after the
//! version gate.

mod harness;

use std::ffi::OsString;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fac::guards::{self, ShellKind};
use fac::shell::install::{self, CMD_FD};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::waitpid;
use nix::unistd::Pid;

const TIMEOUT: Duration = Duration::from_secs(15);

/// A scratch HOME, removed on drop.
struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fac-launch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creating the scratch HOME");
        Self(dir)
    }

    fn write(&self, name: &str, text: &str) {
        let path = self.0.join(name);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        std::fs::write(path, text).expect("writing an rc file");
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A shell started from fac's launch spec on its own PTY.
struct Shell {
    /// `None` once dropped.
    master: Option<OwnedFd>,
    pid: Pid,
    output: Vec<u8>,
}

impl Shell {
    /// Start the `kind` shell under `home` as fac would once the gate lets
    /// it run: `env -i` plus the scratch HOME and XDG dirs, fac's env for
    /// the shell, and the command pipe at fd 63.
    fn start(kind: ShellKind, home: &Home) -> Shell {
        let shell = harness::shell_path(kind);
        let cache = home.0.join("Library/Caches/fac");
        let gate = guards::gate(Some(kind), |kind| {
            guards::shell_version(&shell, kind, Some(&cache))
        });
        assert_eq!(
            gate,
            Ok(()),
            "{} is not a supported version",
            shell.display()
        );

        let setup = install::prepare_in(kind, &shell, &cache.join("shell"), None)
            .expect("writing the scripts");
        let mut env: Vec<(OsString, OsString)> = vec![
            ("HOME".into(), home.0.clone().into()),
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "en_US.UTF-8".into()),
            ("XDG_CONFIG_HOME".into(), home.0.join(".config").into()),
            ("XDG_DATA_HOME".into(), home.0.join(".local/share").into()),
            ("XDG_CACHE_HOME".into(), home.0.join(".cache").into()),
        ];
        env.extend(guards::child_env(std::process::id(), "/dev/ttys999"));
        env.extend(setup.env);
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 200,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let child = fac::pty::spawn_process(
            &shell,
            &setup.launch.args,
            &setup.launch.argv0,
            &env,
            size,
            vec![(setup.cmd_reader, CMD_FD)],
        )
        .expect("spawning the shell");
        Shell {
            master: Some(child.master),
            pid: child.pid,
            output: Vec::new(),
        }
    }

    /// Read until `done` holds for the output so far, answering the
    /// terminal queries the shell sends (DA1, CPR).
    fn read_until(&mut self, what: &str, done: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        let mut buf = [0u8; 4096];
        while !done(&String::from_utf8_lossy(&self.output)) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}:\n{}",
                String::from_utf8_lossy(&self.output)
            );
            let master = self.master();
            let mut fds = [PollFd::new(master.as_fd(), PollFlags::POLLIN)];
            match poll(&mut fds, PollTimeout::from(100u16)) {
                Ok(0) | Err(Errno::EINTR) => continue,
                Ok(_) => {}
                Err(err) => panic!("poll: {err}"),
            }
            let n = match nix::unistd::read(master, &mut buf) {
                Ok(n) => n,
                Err(Errno::EAGAIN | Errno::EINTR) => continue,
                Err(err) => panic!("reading the shell's output: {err}"),
            };
            let read = &buf[..n];
            for (query, reply) in [
                (&b"\x1b[c"[..], &b"\x1b[?62;22c"[..]),
                (b"\x1b[0c", b"\x1b[?62;22c"),
                (b"\x1b[6n", b"\x1b[1;1R"),
            ] {
                if read.windows(query.len()).any(|window| window == query) {
                    self.send(reply);
                }
            }
            self.output.extend_from_slice(read);
        }
    }

    fn master(&self) -> &OwnedFd {
        self.master.as_ref().expect("the master is open")
    }

    fn send(&self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            match nix::unistd::write(self.master(), rest) {
                Ok(n) => rest = &rest[n..],
                Err(Errno::EAGAIN | Errno::EINTR) => std::thread::sleep(Duration::from_millis(1)),
                Err(err) => panic!("writing to the shell: {err}"),
            }
        }
    }

    /// Wait for the first prompt (`% `), run `line`, and return the lines
    /// it printed: everything up to a line `END`, which `line` must end
    /// with printing.
    fn run(&mut self, line: &str) -> Vec<String> {
        self.read_until("the prompt", |output| output.contains("% "));
        let start = self.output.len();
        self.send(format!("{line}\r").as_bytes());
        self.read_until("END", |output| {
            output
                .get(start..)
                .is_some_and(|output| output.lines().any(|line| line.trim_end() == "END"))
        });
        // What a line shows: the text after its last carriage return, with
        // no OSC of a hook (bash's 133;C and 7792 come before the output).
        strip_osc(&String::from_utf8_lossy(&self.output[start..]))
            .lines()
            .map(|line| {
                line.rsplit('\r')
                    .next()
                    .unwrap_or(line)
                    .trim_end()
                    .to_owned()
            })
            .skip(1)
            .take_while(|line| line != "END")
            .collect()
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = kill(self.pid, Signal::SIGKILL);
        // Closed first: an exiting shell waits for its unread output to
        // drain.
        self.master = None;
        let _ = waitpid(self.pid, None);
    }
}

/// The line that starts with `prefix`.
/// `text` without its OSC sequences (ended by BEL or ST).
fn strip_osc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\x1b]") {
        out.push_str(&rest[..at]);
        let osc = &rest[at..];
        let end = [("\x07", 1), ("\x1b\\", 2)]
            .into_iter()
            .filter_map(|(end, len)| osc.find(end).map(|i| i + len))
            .min();
        rest = end.map_or("", |end| &osc[end..]);
    }
    out.push_str(rest);
    out
}

fn line<'a>(lines: &'a [String], prefix: &str) -> &'a str {
    lines
        .iter()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("no line {prefix}…: {lines:#?}"))
}

/// Whether `bash` has the `fdflags` loadable fac.bash uses to close fd 63
/// in the shell's children.
fn has_fdflags(bash: &Path) -> bool {
    bash.parent()
        .is_some_and(|bin| bin.join("../lib/bash/fdflags").is_file())
}

/// What bash prints: what the login file saw, what is left after startup,
/// and the shell's children's fds.
const BASH_PROBE: &str = r#"echo "A:$mark"; echo "B:plbq=${PROCESS_LAUNCHED_BY_Q-unset} cmdfd=${FAC_CMD_FD-unset} kept=$__fac_cmd_fd login=$(shopt -q login_shell && echo on || echo off)"; echo "C:$(/bin/ls /dev/fd | /usr/bin/tr '\n' ' ')"; echo EN''D"#;

#[test]
fn bash_runs_fac_bash_and_the_login_files() {
    let home = Home::new("bash");
    home.write(
        ".bash_profile",
        "PS1='% '\nunset HISTFILE\nmark=\"from=bash_profile plbq=${PROCESS_LAUNCHED_BY_Q-unset} cmdfd=${FAC_CMD_FD-unset}\"\n",
    );
    // Only the first of the login files is read.
    home.write(".profile", "mark=from=profile\n");
    home.write(".bashrc", "mark=from=bashrc\n");
    let mut bash = Shell::start(ShellKind::Bash, &home);
    let lines = bash.run(BASH_PROBE);
    // The login file ran with Kiro's flag and the pipe's fd still set.
    assert_eq!(line(&lines, "A:"), "A:from=bash_profile plbq=1 cmdfd=63");
    // fac.bash took them out of the environment afterwards, keeping the fd
    // for the hook; the shell is not a login shell.
    assert_eq!(
        line(&lines, "B:"),
        "B:plbq=unset cmdfd=unset kept=63 login=off"
    );
    let fds = line(&lines, "C:");
    if has_fdflags(&harness::shell_path(ShellKind::Bash)) {
        assert!(!fds.split(' ').any(|fd| fd == "63"), "{fds}");
    }
}

#[test]
fn bash_falls_back_to_dot_profile() {
    let home = Home::new("bash-profile");
    home.write(
        ".profile",
        "PS1='% '\nunset HISTFILE\nmark=\"from=profile plbq=${PROCESS_LAUNCHED_BY_Q-unset}\"\n",
    );
    let mut bash = Shell::start(ShellKind::Bash, &home);
    let lines = bash.run(BASH_PROBE);
    assert_eq!(line(&lines, "A:"), "A:from=profile plbq=1");
}

#[test]
fn fish_runs_config_fish_then_fac_fish() {
    let home = Home::new("fish");
    home.write(
        ".config/fish/config.fish",
        "set -g fish_greeting\nset -g fish_history ''\nfunction fish_prompt; printf '%% '; end\n\
         set -g mark \"plbq=$PROCESS_LAUNCHED_BY_Q cmdfd=$FAC_CMD_FD\"\n\
         if status is-login; set -g login yes; end\n",
    );
    for dir in [
        ".local/share/fish/generated_completions",
        ".cache/fish/generated_completions",
    ] {
        std::fs::create_dir_all(home.0.join(dir)).expect("mkdir");
    }
    let mut fish = Shell::start(ShellKind::Fish, &home);
    let lines = fish.run(
        r#"echo "A:$mark"; echo "B:plbq="(set -q PROCESS_LAUNCHED_BY_Q; and echo set; or echo unset)" cmdfd="(set -q FAC_CMD_FD; and echo set; or echo unset)" kept=$__fac_cmd_fd login=$login"; echo EN''D"#,
    );
    // config.fish ran first, with Kiro's flag and the pipe's fd set.
    assert_eq!(line(&lines, "A:"), "A:plbq=1 cmdfd=63");
    // Then fac.fish took them out of the environment, keeping the fd for
    // the hook; `-fish` is a login shell.
    assert_eq!(
        line(&lines, "B:"),
        "B:plbq=unset cmdfd=unset kept=63 login=yes"
    );
}
