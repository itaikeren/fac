//! `fac theme new`, run as the binary with a temp `HOME`.

use std::path::PathBuf;
use std::process::{Command, Output};

struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fac-theme-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creating the temp HOME");
        Self(dir)
    }

    fn fac(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fac"))
            .args(args)
            .env("HOME", &self.0)
            .output()
            .expect("running fac")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0
            .join(".config/fac/themes")
            .join(format!("{name}.json"))
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn writes_the_file_and_says_what_to_add() {
    let home = Home::new("write");
    let out = home.fac(&["theme", "new", "mine"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains(&home.file("mine").display().to_string()),
        "{stdout}"
    );
    assert!(stdout.contains("theme = \"mine\""), "{stdout}");
    let written = std::fs::read_to_string(home.file("mine")).expect("the theme file");
    let printed = home.fac(&["theme", "new", "x", "--stdout"]);
    assert_eq!(written.trim_end(), text(&printed.stdout).trim_end());
    assert!(!home.file("x").exists(), "--stdout writes nothing");
    let light = home.fac(&["theme", "new", "x", "--light", "--stdout"]);
    assert_ne!(text(&light.stdout), text(&printed.stdout));
}

#[test]
fn refuses_to_overwrite_and_bad_names() {
    let home = Home::new("refuse");
    assert!(home.fac(&["theme", "new", "mine"]).status.success());
    let before = std::fs::read_to_string(home.file("mine")).expect("the theme file");
    let again = home.fac(&["theme", "new", "mine", "--light"]);
    assert!(!again.status.success());
    assert_eq!(
        text(&again.stderr).trim().lines().count(),
        1,
        "{}",
        text(&again.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(home.file("mine")).expect("kept"),
        before
    );
    for name in ["../evil", "a/b", "x y"] {
        let out = home.fac(&["theme", "new", name]);
        assert!(!out.status.success(), "{name}");
    }
    assert!(!home.0.join(".config/fac/evil.json").exists());
    assert!(!home.fac(&["theme"]).status.success());
}
