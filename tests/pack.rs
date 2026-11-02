//! Tests of the spec packer (`fac-pack-specs`), run with `--features pack`.
//! `packing_is_deterministic` needs the real package: it is ignored unless
//! asked for, and reads the extracted `build/` dir from `FAC_FIG_BUILD`.

#![cfg(feature = "pack")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fac::specs::archive::{Archive, Kind};
use fac::specs::model::{Entry, FnRef, LoadSpec, Script, StringOrFn};

/// A fresh, empty directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn put(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
    fs::write(path, text).expect("write module");
}

fn pack(build: &Path, overrides: &Path, out: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fac-pack-specs"))
        .args([build, overrides, out])
        .output()
        .expect("run fac-pack-specs")
}

fn pack_ok(build: &Path, overrides: &Path, out: &Path) -> Archive {
    let output = pack(build, overrides, out);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = fs::read(out.join("fig.zst")).expect("fig.zst");
    let index = fs::read_to_string(out.join("index.txt")).expect("index.txt");
    Archive::from_parts(data, &index).expect("index parses")
}

fn entry(archive: &Archive, name: &str) -> Entry {
    match archive.entry(name) {
        Some(Ok(entry)) => entry,
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn an_override_wins() {
    let dir = scratch("an_override_wins");
    let (build, overrides, out) = (dir.join("build"), dir.join("overrides"), dir.join("out"));
    put(
        &build,
        "git.js",
        r#"export default {name:"git",description:"vendored"}"#,
    );
    put(&build, "ls.js", r#"export default {name:"ls"}"#);
    put(
        &overrides,
        "git.js",
        r#"export default {name:"git",description:"override"}"#,
    );
    put(
        &overrides,
        "extra/tool.js",
        r#"export default {name:"tool"}"#,
    );
    let archive = pack_ok(&build, &overrides, &out);

    let names: Vec<&str> = archive.names().collect();
    assert_eq!(names, ["extra/tool", "git", "ls"]);
    let git = entry(&archive, "git");
    let root = git.root().expect("a spec");
    assert_eq!(root.description.as_deref(), Some("override"));
    let report = fs::read_to_string(out.join("pack-report.txt")).expect("report");
    assert!(report.contains("git.js replaces git"), "{report}");
    assert!(report.contains("extra/tool.js adds extra/tool"), "{report}");
}

#[test]
fn functions_are_numbered_by_path() {
    let dir = scratch("functions_are_numbered_by_path");
    let (build, out) = (dir.join("build"), dir.join("out"));
    // The default export is not the first export, and a generator object is
    // shared by two args.
    put(
        &build,
        "tool.js",
        r#"var g={script:(t)=>["ls",t[0]],postProcess:(o)=>[]};
var c={name:"tool",
  args:{generators:g,parserDirectives:{alias:async()=>"x"}},
  subcommands:[{name:"sub",args:[{},{generators:[{custom:async()=>[]},g]}],loadSpec:()=>"y"}],
  options:[{name:"-o",args:{suggestions:["a"],isOptional:true,description:undefined}}]};
export{g as shared,c as default};"#,
    );
    put(
        &build,
        "ver/index.js",
        r#"export default (v)=>({versionedSpecPath:"ver/1.0.0",version:v});
export const getVersionCommand=async()=>"1.0.0";"#,
    );
    put(
        &build,
        "ver/1.0.0.js",
        r#"var s={name:"ver",args:{generators:{custom:async()=>[]}}};
var v={"1.1.0":{subcommands:[{name:"new",generateSpec:async()=>({name:"x"})}]},"1.2.0":{subcommands:[{name:"new",remove:true}]}};
export{s as default,v as versions};"#,
    );
    put(
        &build,
        "plain.js",
        r#"export default {name:"plain",args:{template:"filepaths"}}"#,
    );
    let archive = pack_ok(&build, &dir.join("none"), &out);

    let Entry::Spec { root, fns } = entry(&archive, "tool") else {
        panic!("tool is not a spec");
    };
    assert_eq!(
        fns,
        [
            "args.generators.script",
            "args.generators.postProcess",
            "args.parserDirectives.alias",
            "subcommands.0.args.1.generators.0.custom",
            "subcommands.0.args.1.generators.1.script",
            "subcommands.0.args.1.generators.1.postProcess",
            "subcommands.0.loadSpec",
        ]
    );
    let generator = &root.args[0].generators[0];
    assert_eq!(generator.script, Some(Script::Fn(FnRef(0))));
    assert_eq!(generator.post_process, Some(FnRef(1)));
    assert_eq!(
        root.args[0]
            .parser_directives
            .as_ref()
            .and_then(|d| d.alias.clone()),
        Some(StringOrFn::Fn(FnRef(2)))
    );
    let sub = &root.subcommands[0];
    assert_eq!(sub.args.len(), 2);
    assert_eq!(sub.args[1].generators[0].custom, Some(FnRef(3)));
    assert_eq!(sub.args[1].generators[1].script, Some(Script::Fn(FnRef(4))));
    assert_eq!(sub.load_spec, Some(LoadSpec::Fn(FnRef(6))));
    assert_eq!(root.options[0].args[0].description, None);
    assert!(archive.source("tool").is_some());

    assert_eq!(archive.kind("ver"), Some(Kind::Versioned));
    assert_eq!(entry(&archive, "ver").fns(), [""]);
    let Entry::VersionFile { fns, versions, .. } = entry(&archive, "ver/1.0.0") else {
        panic!("ver/1.0.0 is not a version file");
    };
    assert_eq!(
        fns,
        [
            "args.generators.custom",
            "versions.1.1.0.subcommands.0.generateSpec"
        ]
    );
    assert_eq!(
        versions,
        r#"{"1.1.0":{"subcommands":[{"name":"new","generateSpec":{"$fn":1}}]},"1.2.0":{"subcommands":[{"name":"new","remove":true}]}}"#
    );

    let plain = entry(&archive, "plain");
    assert!(!plain.has_functions());
    assert!(archive.source("plain").is_none());
}

#[test]
fn skips_and_failures() {
    let dir = scratch("skips_and_failures");
    let (build, out) = (dir.join("build"), dir.join("out"));
    put(&build, "index.js", r#"export default ["a"]"#);
    put(&build, "helper.js", r#"export default ["us-east-1"]"#);
    put(&build, "nodefault.js", "export const x = 1;");
    put(&build, "a.js", r#"export default {name:"a"}"#);
    let archive = pack_ok(&build, &dir.join("none"), &out);
    assert_eq!(archive.names().collect::<Vec<_>>(), ["a"]);
    let report = fs::read_to_string(out.join("pack-report.txt")).expect("report");
    for line in [
        "Source modules (.js files in the build dir): 4",
        "skipped:                3",
        "helper.js: the default export is an array, not a spec",
        "index.js: the package's list of spec names, not a spec",
        "nodefault.js: no default export",
    ] {
        assert!(report.contains(line), "{line}\n{report}");
    }

    put(&build, "broken.js", "throw new Error('boom')");
    let output = pack(&build, &dir.join("none"), &dir.join("out2"));
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("broken.js") && stderr.contains("boom"),
        "{stderr}"
    );

    fs::remove_file(build.join("broken.js")).expect("remove");
    put(
        &build,
        "typo.js",
        r#"export default {name:"typo",descripton:"x"}"#,
    );
    let output = pack(&build, &dir.join("none"), &dir.join("out3"));
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("(root).descripton"), "{stderr}");
}

#[test]
#[ignore = "needs FAC_FIG_BUILD, the extracted package's build/ dir"]
fn packing_is_deterministic() {
    let build = PathBuf::from(std::env::var("FAC_FIG_BUILD").expect("FAC_FIG_BUILD"));
    let dir = scratch("packing_is_deterministic");
    let overrides = Path::new(env!("CARGO_MANIFEST_DIR")).join("specs/overrides");
    let (one, two) = (dir.join("one"), dir.join("two"));
    pack_ok(&build, &overrides, &one);
    pack_ok(&build, &overrides, &two);
    for file in ["fig.zst", "index.txt", "pack-report.txt"] {
        let a = fs::read(one.join(file)).expect("read");
        let b = fs::read(two.join(file)).expect("read");
        assert!(a == b, "{file} differs between two packs");
    }

    // The categories add up to every .js file in the build dir.
    let report = fs::read_to_string(one.join("pack-report.txt")).expect("report");
    let count = |label: &str| -> usize {
        report
            .lines()
            .find_map(|line| line.trim_start().strip_prefix(label))
            .and_then(|rest| rest.trim().parse().ok())
            .unwrap_or_else(|| panic!("no {label} in\n{report}"))
    };
    let total = count("Source modules (.js files in the build dir):");
    let sum = count("spec without functions:")
        + count("spec with functions:")
        + count("versioned:")
        + count("version file:")
        + count("skipped:");
    assert_eq!(sum, total);
    fn js_files(dir: &Path) -> usize {
        fs::read_dir(dir)
            .expect("read dir")
            .map(|item| item.expect("entry").path())
            .map(|path| {
                if path.is_dir() {
                    js_files(&path)
                } else {
                    usize::from(path.extension().is_some_and(|ext| ext == "js"))
                }
            })
            .sum()
    }
    assert_eq!(total, js_files(&build));
}
