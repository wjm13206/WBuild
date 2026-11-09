//! End-to-end CLI tests. These exercise the real binary, a real C compiler and
//! the on-disk state/cache, so they are skipped automatically on machines
//! without a usable C toolchain.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn wbuild() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wbuild"))
}

fn run(dir: &Path, args: &[&str]) -> Output {
    wbuild()
        .current_dir(dir)
        .args(args)
        .output()
        .expect("failed to run wbuild")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n")
}

fn have_cc() -> bool {
    ["gcc", "cc", "clang"].iter().any(|prog| {
        Command::new(prog)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn c_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("wbuild.toml"),
        r#"[project]
name = "e2e"
out_dir = "build"

[target.app]
kind = "exe"
sources = ["src/*.c"]
include = ["include"]
test = true
"#,
    );
    write(
        &dir.path().join("src/main.c"),
        "#include \"value.h\"\n#include <stdio.h>\nint main(int argc, char **argv){ printf(\"%d\\n\", value()); if (argc > 1) printf(\"arg=%s\\n\", argv[1]); return 0; }\n",
    );
    write(
        &dir.path().join("src/value.c"),
        "#include \"value.h\"\nint value(void){ return 7; }\n",
    );
    write(
        &dir.path().join("include/value.h"),
        "#ifndef VALUE_H\n#define VALUE_H\nint value(void);\n#endif\n",
    );
    dir
}

#[test]
fn builds_incrementally_and_uses_cache() {
    if !have_cc() {
        eprintln!("skipping: no C compiler on PATH");
        return;
    }
    let dir = c_project();
    let exe = dir
        .path()
        .join("build")
        .join("debug")
        .join(if cfg!(windows) { "app.exe" } else { "app" });

    let first = run(dir.path(), &["build"]);
    assert!(
        first.status.success(),
        "first build failed: {}",
        stderr(&first)
    );
    assert!(exe.is_file(), "missing artifact {}", exe.display());

    let second = run(dir.path(), &["build", "--explain"]);
    assert!(second.status.success());
    let text = stdout(&second);
    assert!(
        text.contains("up-to-date"),
        "expected a no-op build:\n{text}"
    );

    // Touching a header must rebuild the translation units that include it.
    write(
        &dir.path().join("include/value.h"),
        "#ifndef VALUE_H\n#define VALUE_H\n/* changed */\nint value(void);\n#endif\n",
    );
    let third = run(dir.path(), &["build", "--explain"]);
    assert!(third.status.success());
    let text = stdout(&third);
    assert!(
        text.contains("built"),
        "header change did not rebuild:\n{text}"
    );
    assert!(
        text.contains("inputs or command changed"),
        "expected a reason for the rebuild:\n{text}"
    );

    // Removing the outputs but keeping the cache must restore them without
    // invoking the compiler.
    let clean = run(dir.path(), &["clean"]);
    assert!(clean.status.success());
    let restored = run(dir.path(), &["build", "--explain"]);
    assert!(restored.status.success(), "{}", stderr(&restored));
    assert!(exe.is_file());
    let text = stdout(&restored);
    assert!(
        text.contains("cached"),
        "expected cache hits after `clean`:\n{text}"
    );
}

#[test]
fn init_scaffolds_a_buildable_project() {
    if !have_cc() {
        eprintln!("skipping: no C compiler on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let init = run(dir.path(), &["init", ".", "--name", "scaffold"]);
    assert!(init.status.success(), "{}", stderr(&init));
    assert!(dir.path().join("wbuild.toml").is_file());
    assert!(dir.path().join("src/main.c").is_file());

    let build = run(dir.path(), &["build"]);
    assert!(build.status.success(), "{}", stderr(&build));
}

#[test]
fn run_and_test_commands() {
    if !have_cc() {
        eprintln!("skipping: no C compiler on PATH");
        return;
    }
    let dir = c_project();

    let run_out = run(dir.path(), &["run", "app", "--", "hello"]);
    assert!(
        run_out.status.success(),
        "`run` failed: {}",
        stderr(&run_out)
    );
    let text = stdout(&run_out);
    assert!(text.contains("running app"), "missing run banner:\n{text}");
    assert!(
        text.contains("arg=hello"),
        "argument was not forwarded:\n{text}"
    );

    let test_out = run(dir.path(), &["test"]);
    assert!(
        test_out.status.success(),
        "`test` failed: {}",
        stderr(&test_out)
    );
    let text = stdout(&test_out);
    assert!(text.contains("test app"), "missing test banner:\n{text}");
    assert!(
        text.contains("all 1 test target(s) passed"),
        "missing test summary:\n{text}"
    );
}

#[test]
fn object_target_can_be_linked_by_an_executable() {
    if !have_cc() {
        eprintln!("skipping: no C compiler on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("wbuild.toml"),
        r#"[project]
name = "obj"
out_dir = "build"

[target.piece]
kind = "object"
sources = ["one.c"]

[target.app]
kind = "exe"
sources = ["main.c"]
links = ["piece"]
"#,
    );
    write(&dir.path().join("one.c"), "int one(void){ return 1; }\n");
    write(
        &dir.path().join("main.c"),
        "int one(void);\nint main(void){ return one() == 1 ? 0 : 1; }\n",
    );

    let out = run(dir.path(), &["build"]);
    assert!(out.status.success(), "build failed: {}", stderr(&out));
    assert!(
        dir.path().join("build/debug/obj/piece/one.o").is_file()
            || dir.path().join("build/debug/obj/piece/one.obj").is_file(),
        "object file was not produced"
    );
    let exe = dir
        .path()
        .join("build")
        .join("debug")
        .join(if cfg!(windows) { "app.exe" } else { "app" });
    assert!(exe.is_file(), "executable was not linked");
}

#[test]
fn custom_target_tracks_inputs_and_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let (program, args) = if cfg!(windows) {
        ("cmd", r#"["/C", "copy", "input.txt", "output.txt"]"#)
    } else {
        ("cp", r#"["input.txt", "output.txt"]"#)
    };
    write(
        &dir.path().join("wbuild.toml"),
        &format!(
            r#"[project]
name = "custom"
out_dir = "build"

[target.gen]
kind = "custom"
program = "{program}"
args = {args}
inputs = ["input.txt"]
outputs = ["output.txt"]
"#
        ),
    );
    write(&dir.path().join("input.txt"), "first\n");

    let first = run(dir.path(), &["build"]);
    assert!(first.status.success(), "build failed: {}", stderr(&first));
    let output = dir.path().join("output.txt");
    assert!(output.is_file(), "custom output is missing");
    assert_eq!(fs::read_to_string(&output).unwrap(), "first\n");

    let second = run(dir.path(), &["build"]);
    assert!(second.status.success());
    assert!(
        stdout(&second).contains("up-to-date"),
        "custom target should be incrementally up to date:\n{}",
        stdout(&second)
    );

    write(&dir.path().join("input.txt"), "second\n");
    let third = run(dir.path(), &["build"]);
    assert!(third.status.success());
    assert!(
        stdout(&third).contains("built"),
        "changing an input should rerun the custom target:\n{}",
        stdout(&third)
    );
    assert_eq!(fs::read_to_string(&output).unwrap(), "second\n");
}

#[test]
fn inspection_commands_report_state() {
    if !have_cc() {
        eprintln!("skipping: no C compiler on PATH");
        return;
    }
    let dir = c_project();

    let configure = run(dir.path(), &["configure"]);
    assert!(configure.status.success());
    assert!(stdout(&configure).contains("profile   debug"));

    let version = run(dir.path(), &["version"]);
    assert!(version.status.success());
    assert!(stdout(&version).contains("WBuild 0.1.0"));

    let build = run(dir.path(), &["build", "--timings"]);
    assert!(build.status.success(), "{}", stderr(&build));

    let stats = run(dir.path(), &["cache", "stats"]);
    assert!(stats.status.success());
    assert!(stdout(&stats).contains("objects"));

    let cache_dir = run(dir.path(), &["cache", "dir"]);
    assert!(cache_dir.status.success());
    assert!(stdout(&cache_dir).contains(".wbuild"));

    let timings = run(dir.path(), &["profile"]);
    assert!(timings.status.success(), "{}", stderr(&timings));
    let text = stdout(&timings);
    assert!(text.contains("profile debug"), "unexpected report:\n{text}");
    assert!(text.contains("CC "), "missing step entries:\n{text}");
}

#[test]
fn generated_sources_are_built_before_consumers() {
    if !have_cc() {
        eprintln!("skipping: no C compiler on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (program, args) = if cfg!(windows) {
        (
            "cmd",
            "\"/C\", \"copy\", \"/Y\", \"part_template.c\", \"generated.c\"",
        )
    } else {
        ("cp", "\"-f\", \"part_template.c\", \"generated.c\"")
    };
    write(
        &dir.path().join("wbuild.toml"),
        &format!(
            r#"[project]
name = "generated"

[target.gen]
kind = "custom"
program = "{program}"
args = [{args}]
inputs = ["part_template.c"]
outputs = ["generated.c"]

[target.app]
kind = "exe"
sources = ["main.c", "generated.c"]
links = ["gen"]
"#
        ),
    );
    write(
        &dir.path().join("part_template.c"),
        "int part(void){ return 7; }\n",
    );
    write(
        &dir.path().join("main.c"),
        "#include <stdio.h>\nint part(void);\nint main(void){ printf(\"%d\\n\", part()); return 0; }\n",
    );

    let first = run(dir.path(), &["build"]);
    assert!(
        first.status.success(),
        "build with a generated source failed: {}",
        stderr(&first)
    );
    let app = dir
        .path()
        .join("build/debug")
        .join(if cfg!(windows) { "app.exe" } else { "app" });
    assert!(app.is_file(), "missing artifact {}", app.display());

    let executed = run(dir.path(), &["run", "app"]);
    assert!(executed.status.success(), "{}", stderr(&executed));
    assert!(stdout(&executed).contains('7'));

    let second = run(dir.path(), &["build"]);
    assert!(second.status.success());
    assert!(
        stdout(&second).contains("up-to-date"),
        "generated pipeline should be incrementally up to date:\n{}",
        stdout(&second)
    );
}

#[test]
fn unknown_target_is_reported() {
    let dir = c_project();
    let out = run(dir.path(), &["build", "no-such-target"]);
    assert!(!out.status.success());
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        text.contains("no-such-target"),
        "error should mention the target:\n{text}"
    );
}
