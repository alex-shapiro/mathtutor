//! Tests that run the `mt` binary.

use std::process::{Command, Output};

mod common;

/// Run `mt` with `args` and without any Baserunna setting.
fn mt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mt"))
        .args(args)
        .env_remove("BASERUNNA_TOKEN")
        .env_remove("MT_GRAPH")
        .env("MATHTUTOR_HOME", std::env::temp_dir())
        .output()
        .expect("run mt")
}

#[test]
fn commands_without_learner_state_run_without_a_database() {
    for args in [
        &["instruct"][..],
        &["graph", "show", "fnd.1"],
        &["graph", "list", "fnd.1", "--recursive"],
        &["graph", "check"],
    ] {
        let out = mt(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "mt {args:?}: {stderr}");
    }
}

#[test]
fn commands_with_learner_state_require_a_database_token() {
    for args in [
        &["path", "list"][..],
        &["path", "new", "g", "--targets", "fnd.1.1.1"],
        &["graph", "show", "fnd.1", "--path", "p"],
        &["graph", "dump"],
    ] {
        let out = mt(args);
        assert_eq!(out.status.code(), Some(2), "mt {args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("BASERUNNA_TOKEN"), "mt {args:?}: {stderr}");
    }
}

#[test]
fn top_level_graph_option_selects_the_curriculum_of_every_command() {
    let dir = common::write_graph(
        "
  - id: ta.1
    name: cluster
    children:
      - id: ta.1.1
        name: local atom
        description: d
",
    );
    let dir = dir.path().to_str().unwrap();
    let show = mt(&["--graph", dir, "graph", "show", "ta.1.1"]);
    assert!(show.status.success());
    assert!(String::from_utf8_lossy(&show.stdout).contains("local atom"));

    let check = mt(&["--graph", dir, "graph", "check"]);
    assert_eq!(check.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(stdout.contains("orphan atom 'local atom'"), "{stdout}");
    let stderr = String::from_utf8_lossy(&check.stderr);
    assert!(stderr.contains("curriculum check failed"), "{stderr}");
}
