//! `fac spec new`, `fac spec check` and `fac spec list`, run as the binary
//! with a temp `HOME`.

use std::path::PathBuf;
use std::process::{Command, Output};

struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fac-spec-cli-{tag}-{}", std::process::id()));
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

    fn specs(&self) -> PathBuf {
        self.0.join(".config/fac/specs")
    }

    fn write(&self, file: &str, text: &str) -> PathBuf {
        let path = self.specs().join(file);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        std::fs::write(&path, text).expect("writing a spec");
        path
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

/// Stdout and stderr, for assert messages.
fn both(out: &Output) -> String {
    format!(
        "status {:?}\n--- stdout ---\n{}--- stderr ---\n{}",
        out.status.code(),
        text(&out.stdout),
        text(&out.stderr)
    )
}

#[test]
fn new_writes_each_format_and_says_it_is_live() {
    let home = Home::new("new");
    for (args, file) in [
        (&["spec", "new", "mytool"][..], "mytool.ts"),
        (&["spec", "new", "other", "--js"][..], "other.js"),
        (
            &["spec", "new", "tools/third", "--json"][..],
            "tools/third.json",
        ),
    ] {
        let out = home.fac(args);
        assert!(out.status.success(), "{}", both(&out));
        let stdout = text(&out.stdout);
        let path = home.specs().join(file);
        assert!(path.is_file(), "{file}");
        assert!(stdout.contains(&path.display().to_string()), "{stdout}");
        assert!(stdout.contains("live at your next prompt"), "{stdout}");
        // Each starter loads.
        let check = home.fac(&["spec", "check", &path.display().to_string()]);
        assert!(check.status.success(), "{file}: {}", both(&check));
    }
    let ts = text(&home.fac(&["spec", "new", "x"]).stdout);
    assert!(!ts.contains("fig.d.ts"), "written once only: {ts}");
    assert!(home.specs().join("fig.d.ts").is_file());
    assert!(home.specs().join("tsconfig.json").is_file());
    let starter = std::fs::read_to_string(home.specs().join("mytool.ts")).expect("the starter");
    for part in [
        "name: \"mytool\"",
        "subcommands:",
        "--config",
        "--verbose",
        "script:",
        "filepaths(",
        "@fig/autocomplete-generators",
        "index.d.ts",
    ] {
        assert!(
            starter.contains(part),
            "{part} is not in the starter:\n{starter}"
        );
    }
}

#[test]
fn new_refuses_to_overwrite_and_bad_names() {
    let home = Home::new("refuse");
    assert!(home.fac(&["spec", "new", "mytool"]).status.success());
    home.write("mytool.ts", "mine");
    let again = home.fac(&["spec", "new", "mytool"]);
    assert!(!again.status.success());
    assert_eq!(
        text(&again.stderr).trim().lines().count(),
        1,
        "{}",
        both(&again)
    );
    assert_eq!(
        std::fs::read_to_string(home.specs().join("mytool.ts")).expect("kept"),
        "mine"
    );
    for args in [
        &["spec", "new", "../evil"][..],
        &["spec", "new", "a b"],
        &["spec", "new", "a//b"],
        &["spec", "new", "fig.d"],
        &["spec", "new", "tsconfig", "--json"],
    ] {
        let out = home.fac(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", both(&out));
    }
    for args in [
        &["spec", "new"][..],
        &["spec", "new", "x", "--js", "--json"],
        &["spec", "new", "x", "--yaml"],
        &["spec"],
        &["spec", "list", "extra"],
    ] {
        let out = home.fac(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", both(&out));
    }
}

#[test]
fn check_summarises_a_good_spec_and_warns() {
    let home = Home::new("good");
    home.write(
        "git.ts",
        r#"import { filepaths } from "@fig/autocomplete-generators";
const spec: Fig.Spec = {
  name: "git",
  subcommands: [{ name: "mine", args: { name: "f", generators: filepaths } }],
  options: [{ name: "--x", colour: "red" } as Fig.Option],
  args: { name: "b", generators: { script: ["git", "branch"] } },
};
export default spec;
"#,
    );
    let json = home.write("git.json", r#"{"name": "git"}"#);
    let shadowed = home.fac(&["spec", "check", &json.display().to_string()]);
    assert!(shadowed.status.success(), "{}", both(&shadowed));
    assert!(
        text(&shadowed.stdout).contains("git.ts for git, not this file"),
        "{}",
        both(&shadowed)
    );
    let out = home.fac(&["spec", "check", "git"]);
    assert!(out.status.success(), "{}", both(&out));
    let stdout = text(&out.stdout);
    for line in [
        "git.ts (TypeScript): loads",
        "replaces fac's own spec for git",
        "1 subcommand, 1 option, 2 args, 2 generators (1 runs JavaScript)",
        "git.json is not used",
        "warning: unknown key options.0.colour",
        "warning: args.0.generators.0 runs a script but has no splitOn or postProcess",
        "warning: args.0.generators.0.script: runs `git branch`",
        "warning: subcommands.0.args.0.generators.0.custom: a function that gets executeShellCommand",
    ] {
        assert!(stdout.contains(line), "{line:?} is missing:\n{stdout}");
    }
}

#[test]
fn check_says_exactly_what_is_wrong() {
    let home = Home::new("broken");
    let cases = [
        (
            "syntax.ts",
            "const spec: Fig.Spec = {\n  name: \"x\"\n  args: {},\n};\nexport default spec;\n",
            "syntax.ts:3:3: Expected `,` or `}`",
        ),
        (
            "imports.js",
            "import x from \"lodash\";\nexport default { name: x };\n",
            "imports.js:1:15: it imports \"lodash\"",
        ),
        (
            "typo.json",
            r#"{"name": "typo", "subcommands": [{"name": "a", "hiden": true}]}"#,
            "typo.json: unknown key subcommands.0.hiden",
        ),
        (
            "field.json",
            "{\"name\": \"f\",\n \"args\": {\"isOptional\": \"yes\"}}",
            "field.json:2:",
        ),
        (
            "function.json",
            r#"{"name": "f", "args": {"generators": {"custom": "x"}}}"#,
            "args.generators.custom needs a function",
        ),
        (
            "thrown.ts",
            "const n: number = 1;\nconst f = (): string => {\n  throw new Error(\"boom\");\n};\nexport default { name: f() };\n",
            "(thrown.ts:3:",
        ),
        (
            "nothing.js",
            "export const spec = {};\n",
            "nothing.js: the default export is not a spec (no default export)",
        ),
    ];
    for (file, source, expected) in cases {
        let path = home.write(file, source);
        let out = home.fac(&["spec", "check", &path.display().to_string()]);
        assert_eq!(out.status.code(), Some(1), "{file}: {}", both(&out));
        assert!(
            text(&out.stderr).contains(expected),
            "{file}: {expected:?} is missing:\n{}",
            both(&out)
        );
        assert!(text(&out.stdout).is_empty(), "{file}: {}", both(&out));
    }
    // By name, or by its file name in the folder, the same.
    for target in ["typo", "typo.json"] {
        let out = home.fac(&["spec", "check", target]);
        assert_eq!(out.status.code(), Some(1), "{}", both(&out));
        assert!(text(&out.stderr).contains("unknown key"), "{}", both(&out));
    }
    // No such spec.
    let out = home.fac(&["spec", "check", "absent"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        text(&out.stderr).contains("no spec named \"absent\""),
        "{}",
        both(&out)
    );
}

#[test]
fn check_with_no_argument_checks_every_spec() {
    let home = Home::new("all");
    let empty = home.fac(&["spec", "check"]);
    assert!(empty.status.success(), "{}", both(&empty));
    assert!(
        text(&empty.stdout).contains("fac spec new"),
        "{}",
        both(&empty)
    );
    assert!(home.fac(&["spec", "new", "one"]).status.success());
    assert!(home.fac(&["spec", "new", "two", "--json"]).status.success());
    let out = home.fac(&["spec", "check"]);
    assert!(out.status.success(), "{}", both(&out));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("one.ts (TypeScript): loads"), "{stdout}");
    assert!(stdout.contains("two.json (JSON): loads"), "{stdout}");
    assert!(
        !stdout.contains("fig.d.ts") && !stdout.contains("tsconfig"),
        "{stdout}"
    );
    home.write("three.js", "export default {\n");
    let out = home.fac(&["spec", "check"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(text(&out.stderr).contains("three.js:2:1"), "{}", both(&out));
    assert!(
        text(&out.stderr).contains("1 of 3 specs did not load"),
        "{}",
        both(&out)
    );
}

#[test]
fn list_says_what_each_spec_replaces() {
    let home = Home::new("list");
    let empty = home.fac(&["spec", "list"]);
    assert!(empty.status.success());
    assert!(text(&empty.stdout).contains("No specs"), "{}", both(&empty));
    assert!(home.fac(&["spec", "new", "mytool"]).status.success());
    assert!(home.fac(&["spec", "new", "git", "--js"]).status.success());
    home.write("git.json", "{}");
    home.write("aws/ec2.json", "{}");
    let out = home.fac(&["spec", "list"]);
    assert!(out.status.success(), "{}", both(&out));
    assert_eq!(
        text(&out.stdout).lines().collect::<Vec<_>>(),
        [
            "aws/ec2.json  replaces fac's aws/ec2 spec",
            "git.js  replaces fac's git spec; git.json not used",
            "mytool.ts  adds mytool (fac has no spec for it)",
        ],
        "{}",
        both(&out)
    );
}

#[test]
fn help_lists_the_spec_commands() {
    let home = Home::new("help");
    let out = home.fac(&["--help"]);
    let stdout = text(&out.stdout);
    for command in ["spec new", "spec check", "spec list"] {
        assert!(stdout.contains(command), "{command}: {stdout}");
    }
}
