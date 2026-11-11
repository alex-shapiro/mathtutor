//! Tests that run the `mt` binary.

use std::process::{Command, Output};

use mathtutor::db::DbConfig;
use mathtutor::event_log::{self, EventKind};
use mathtutor::instruct::playbook_steps;
use mathtutor::scheduler::{Action, next_step};
use mathtutor::surface::{Field, Step, Surface};
use mathtutor::types::{Difficulty, Strategy};

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
        #[cfg(feature = "mcp")]
        &["mcp", "tools"],
    ] {
        let out = mt(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "mt {args:?}: {stderr}");
    }
}

#[test]
fn a_closed_stdout_ends_a_command_quietly() {
    for args in [
        &["instruct"][..],
        &["graph", "list", "fnd", "--recursive"],
        &["graph", "check"],
        &["--help"],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let out = Command::new(env!("CARGO_BIN_EXE_mt"))
            .args(args)
            .env_remove("BASERUNNA_TOKEN")
            .env_remove("MT_GRAPH")
            .stdout(writer)
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "mt {args:?}: {stderr}");
        assert!(!stderr.contains("panicked"), "mt {args:?}: {stderr}");
        assert!(!stderr.contains("error:"), "mt {args:?}: {stderr}");
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

/// Run `mt` with `args` against the database of `cfg`.
fn mt_on(cfg: &DbConfig, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mt"))
        .args(args)
        .env("BASERUNNA_URL", &cfg.url)
        .env("BASERUNNA_TOKEN", &cfg.token)
        .env("BASERUNNA_DB", &cfg.db)
        .env("MATHTUTOR_HOME", tempfile::TempDir::new().unwrap().path())
        .env_remove("MT_GRAPH")
        .output()
        .expect("run mt")
}

#[tokio::test]
async fn rejected_new_path_does_not_create_a_database() {
    let cfg = common::fresh_config();
    for args in [
        &["path", "new", "g", "--targets", "nope.1"][..],
        &["path", "new", "Learn fractions"],
    ] {
        let out = mt_on(&cfg, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "mt {args:?}: {stderr}");
        assert!(!stderr.contains("created database"), "{stderr}");
    }
    let raw = baserunna_client::Client::new(cfg.transport().unwrap()).with_token(&cfg.token);
    let info = raw.db(&cfg.db).info().await;
    assert!(
        info.is_err(),
        "a rejected path created the database: {info:?}"
    );
    let list = mt_on(&cfg, &["path", "list"]);
    assert_eq!(
        list.status.code(),
        Some(2),
        "the database must still be missing"
    );
}

/// Run the CLI form of `step` through `sh -c` against the database of `cfg`.
fn run_in_shell(cfg: &DbConfig, step: &Step) {
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_mt"));
    let path = format!(
        "{}:{}",
        bin.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let home = tempfile::TempDir::new().unwrap();
    let line = step.render(Surface::Cli);
    let out = Command::new("sh")
        .args(["-c", &line])
        .env("PATH", path)
        .env("BASERUNNA_URL", &cfg.url)
        .env("BASERUNNA_TOKEN", &cfg.token)
        .env("BASERUNNA_DB", &cfg.db)
        .env("MATHTUTOR_HOME", home.path())
        .env_remove("MT_GRAPH")
        .output()
        .expect("run sh");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{line}: {stderr}");
}

/// Return text for `field` that a shell changes unless the step quotes it.
fn shell_text(field: Field) -> String {
    format!(
        "{field:?}: Solve $x^2 = 4$ for $x$; `x` is $(echo -2).\nIt's \"both\" quote kinds, a \\ and a '\\''.\n"
    )
}

#[tokio::test]
async fn cli_steps_store_free_text_unchanged() {
    let cfg = common::fresh_config();
    let db = common::create_db(&cfg).await;
    let path_id = "p_shell";
    common::seed_path(&db, path_id, &["fnd.1.1.1"], Strategy::TopDown).await;
    let (first, second) = ("fnd.1.1.1", "fnd.1.1.2");
    let fill = |atom: &'static str, quiz: &'static str| {
        move |field: Field| match field {
            Field::Atom => atom.to_string(),
            Field::QuizId => quiz.to_string(),
            Field::PathId => path_id.to_string(),
            Field::Difficulty => "easy".to_string(),
            Field::QuizType => "free_text".to_string(),
            Field::Rating => "good".to_string(),
            _ => shell_text(field),
        }
    };
    let template = |name: &str| {
        playbook_steps()
            .into_iter()
            .find(|(n, _)| *n == name)
            .expect("playbook step")
            .1
    };
    let scheduled = |action: Action| next_step(&action, path_id).expect("next step");

    run_in_shell(&cfg, &template("upsert_lesson").fill(fill(first, "")));
    let create_lesson = Action::CreateLesson {
        atom_id: second.into(),
    };
    run_in_shell(&cfg, &scheduled(create_lesson).fill(fill(second, "")));
    let overlay = common::overlay(&db).await;
    for atom in [first, second] {
        let lesson = overlay.atoms[atom].lesson.as_deref();
        assert_eq!(lesson, Some(shell_text(Field::Body).as_str()));
    }

    run_in_shell(&cfg, &template("create_quiz").fill(fill(first, "")));
    let create_quiz = Action::CreateQuiz {
        atom_id: second.into(),
        difficulty: Difficulty::Medium,
    };
    run_in_shell(&cfg, &scheduled(create_quiz).fill(fill(second, "")));
    let overlay = common::overlay(&db).await;
    for atom in [first, second] {
        let q = &overlay.atoms[atom].quizzes[0];
        assert_eq!(q.question, shell_text(Field::Question));
        assert_eq!(q.answer, shell_text(Field::Answer));
        assert_eq!(q.rubric, Some(shell_text(Field::Rubric)));
    }
    let (q1, q2) = ("fnd.1.1.1.q1", "fnd.1.1.2.u1");

    let update = template("update_quiz").fill(|field| match field {
        Field::Question | Field::Answer | Field::Rubric => format!("new {}", shell_text(field)),
        _ => fill(first, q1)(field),
    });
    run_in_shell(&cfg, &update);
    let q = &common::overlay(&db).await.atoms[first].quizzes[0];
    assert_eq!(q.question, format!("new {}", shell_text(Field::Question)));
    assert_eq!(q.rubric, Some(format!("new {}", shell_text(Field::Rubric))));

    run_in_shell(&cfg, &template("answer_quiz").fill(fill(first, q1)));
    let present_quiz = Action::PresentQuiz {
        quiz_id: q2.into(),
        atom_id: second.into(),
    };
    run_in_shell(&cfg, &scheduled(present_quiz).fill(fill(second, q2)));
    let present_lesson = Action::PresentLesson {
        atom_id: first.into(),
    };
    run_in_shell(&cfg, &scheduled(present_lesson));

    let answers: Vec<_> = event_log::load(&db, path_id)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::QuizAnswered {
                quiz, user_answer, ..
            } => Some((quiz, user_answer)),
            _ => None,
        })
        .collect();
    let reply = Some(shell_text(Field::UserAnswer));
    assert_eq!(
        answers,
        vec![(q1.to_string(), reply.clone()), (q2.to_string(), reply)]
    );
}

#[tokio::test]
async fn path_new_accepts_a_target_inside_another_target() {
    let cfg = common::fresh_config();
    common::create_db(&cfg).await;
    let home = tempfile::TempDir::new().unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_mt"))
            .args(args)
            .env("BASERUNNA_URL", &cfg.url)
            .env("BASERUNNA_TOKEN", &cfg.token)
            .env("BASERUNNA_DB", &cfg.db)
            .env("MATHTUTOR_HOME", home.path())
            .env_remove("MT_GRAPH")
            .output()
            .expect("run mt")
    };
    for args in [
        &["path", "new", "g", "--targets", "la.5,la.5.4"][..],
        &["path", "next"],
    ] {
        let out = run(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "mt {args:?}: {stderr}");
    }
}
