//! Integration tests for `store` commands.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use argh::FromArgs;
use baserunna_client::bytes::Bytes;
use baserunna_client::{BoxFuture, HttpTransport, Transport, TransportError, http};
use mathtutor::cli::Mt;
use mathtutor::db::Db;
use mathtutor::event_log::{self, EventKind};
use mathtutor::graph::Curriculum;
use mathtutor::overlay::QuizDraft;
use mathtutor::surface::Surface;
use mathtutor::types::{Difficulty, QuizType, Rating, Strategy};
use mathtutor::{answer, store};

mod common;

/// Transport that delivers the next transaction commit but loses its response and every later request.
struct LostCommitTransport {
    inner: HttpTransport,
    lose_next_commit: Arc<AtomicBool>,
    offline: Arc<AtomicBool>,
}

impl Transport for LostCommitTransport {
    fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        Box::pin(async move {
            if self.offline.load(Ordering::SeqCst) {
                return Err(TransportError::NotSent("offline".into()));
            }
            let commit = req.uri().path().ends_with("/commit");
            if commit && self.lose_next_commit.swap(false, Ordering::SeqCst) {
                self.inner.send(req).await?;
                self.offline.store(true, Ordering::SeqCst);
                return Err(TransportError::Unknown("response lost".into()));
            }
            self.inner.send(req).await
        })
    }
}

const NO_LESSON_ATOM: &str = "fnd.1.1.2";
const WITH_LESSON_ATOM: &str = "fnd.1.1.1";

async fn fresh_db_with_path() -> (Db, String) {
    let db = common::empty_db().await;
    common::seed_path(&db, common::PATH_ID, &[NO_LESSON_ATOM], Strategy::BottomUp).await;
    (db, common::PATH_ID.into())
}

async fn event_kinds(db: &Db, path_id: &str) -> Vec<EventKind> {
    event_log::load(db, path_id)
        .await
        .expect("load events")
        .iter()
        .map(|e| e.kind.clone())
        .filter(|k| *k != EventKind::PathCreated {})
        .collect()
}

fn authored(atom: &str) -> EventKind {
    let atom = atom.into();
    EventKind::LessonAuthored { atom }
}

fn amended(atom: &str) -> EventKind {
    let atom = atom.into();
    EventKind::LessonAmended { atom }
}

fn taught(atom: &str) -> EventKind {
    let atom = atom.into();
    EventKind::LessonTaught { atom }
}

#[tokio::test]
async fn first_store_on_lesson_less_atom_emits_authored_then_taught() {
    let (db, path_id) = fresh_db_with_path().await;

    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "body".into(), Some(&path_id))
        .await
        .expect("store");

    assert_eq!(
        event_kinds(&db, &path_id).await,
        [authored(NO_LESSON_ATOM), taught(NO_LESSON_ATOM)],
    );
}

#[tokio::test]
async fn second_store_on_same_atom_emits_amended_then_taught() {
    let (db, path_id) = fresh_db_with_path().await;

    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "first".into(), Some(&path_id))
        .await
        .unwrap();
    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "second".into(), Some(&path_id))
        .await
        .expect("second store succeeds (upsert)");

    assert_eq!(
        event_kinds(&db, &path_id).await,
        [
            authored(NO_LESSON_ATOM),
            taught(NO_LESSON_ATOM),
            amended(NO_LESSON_ATOM),
            taught(NO_LESSON_ATOM),
        ],
    );
}

#[tokio::test]
async fn repeated_lesson_upsert_after_a_lost_response_counts_one_teaching() {
    let cfg = common::fresh_config();
    let seeded = common::create_db(&cfg).await;
    common::seed_path(
        &seeded,
        common::PATH_ID,
        &[NO_LESSON_ATOM],
        Strategy::BottomUp,
    )
    .await;
    let lose_next_commit = Arc::new(AtomicBool::new(true));
    let offline = Arc::new(AtomicBool::new(false));
    let transport = LostCommitTransport {
        inner: cfg.transport().unwrap(),
        lose_next_commit: lose_next_commit.clone(),
        offline: offline.clone(),
    };
    let db = Db::with_transport(&cfg, transport);

    let lost = store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "body".into(), Some(common::PATH_ID))
        .await
        .unwrap_err();
    assert!(lost.outcome_is_unknown(), "{lost}");
    let cli = <Mt as FromArgs>::from_args(
        &["mt"],
        &["lesson", "upsert", NO_LESSON_ATOM, "--body", "body"],
    )
    .expect("valid args");
    assert_eq!(
        cli.cmd.operation().unknown_outcome_step(Surface::Cli),
        "repeat the command"
    );
    offline.store(false, Ordering::SeqCst);
    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "body".into(), Some(common::PATH_ID))
        .await
        .expect("repeat");

    assert_eq!(
        event_kinds(&seeded, common::PATH_ID).await,
        [authored(NO_LESSON_ATOM), taught(NO_LESSON_ATOM)],
    );
    let state = common::path_state(&seeded, common::PATH_ID).await;
    assert_eq!(state.projection.taught[NO_LESSON_ATOM].count, 1);
}

#[tokio::test]
async fn same_lesson_body_on_a_second_path_records_only_its_teaching() {
    let (db, path_id) = fresh_db_with_path().await;
    common::seed_path(&db, "p_second", &[NO_LESSON_ATOM], Strategy::BottomUp).await;
    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "body".into(), Some(&path_id))
        .await
        .unwrap();

    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "body".into(), Some("p_second"))
        .await
        .unwrap();
    assert_eq!(event_kinds(&db, "p_second").await, [taught(NO_LESSON_ATOM)]);

    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "body".into(), Some("p_second"))
        .await
        .unwrap();
    assert_eq!(event_kinds(&db, "p_second").await, [taught(NO_LESSON_ATOM)]);
    let state = common::path_state(&db, "p_second").await;
    assert_eq!(state.projection.taught[NO_LESSON_ATOM].count, 1);
}

#[tokio::test]
async fn a_removed_quiz_cannot_be_updated() {
    let (db, path_id) = fresh_db_with_path().await;
    let quiz = format!("{WITH_LESSON_ATOM}.q1");
    store::cmd_quiz_delete(&db, &quiz, Some(&path_id))
        .await
        .unwrap();

    let patch = mathtutor::overlay::QuizPatch {
        question: Some("back?".into()),
        ..Default::default()
    };
    let e = store::cmd_quiz_update(&db, &quiz, &patch, Some(&path_id))
        .await
        .unwrap_err();

    assert!(
        matches!(&e, mathtutor::Error::UnknownId(id) if *id == quiz),
        "{e:?}"
    );
    let overlay = common::overlay(&db).await;
    assert!(overlay.atoms[WITH_LESSON_ATOM].removed.contains(&quiz));
}

#[tokio::test]
async fn quiz_create_after_delete_skips_tombstoned_id() {
    let (db, path_id) = fresh_db_with_path().await;

    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "body".into(), Some(&path_id))
        .await
        .unwrap();
    let first = store::cmd_quiz_create(
        &db,
        NO_LESSON_ATOM,
        QuizDraft {
            difficulty: Difficulty::Easy,
            question: "q1?".into(),
            answer: "a1".into(),
            rubric: None,
            kind: QuizType::FreeText,
        },
        Some(&path_id),
    )
    .await
    .unwrap();
    assert_eq!(first, format!("{NO_LESSON_ATOM}.u1"));

    store::cmd_quiz_delete(&db, &first, Some(&path_id))
        .await
        .unwrap();

    let second = store::cmd_quiz_create(
        &db,
        NO_LESSON_ATOM,
        QuizDraft {
            difficulty: Difficulty::Easy,
            question: "q2?".into(),
            answer: "a2".into(),
            rubric: None,
            kind: QuizType::FreeText,
        },
        Some(&path_id),
    )
    .await
    .expect("create after delete must not recycle the tombstoned id");
    assert_eq!(second, format!("{NO_LESSON_ATOM}.u2"));
}

#[tokio::test]
async fn a_user_quiz_keeps_its_id_apart_from_the_curriculum_quizzes() {
    let (db, path_id) = fresh_db_with_path().await;
    let atom = WITH_LESSON_ATOM;
    let draft = QuizDraft {
        difficulty: Difficulty::Hard,
        question: "q?".into(),
        answer: "a".into(),
        rubric: None,
        kind: QuizType::FreeText,
    };
    let created = store::cmd_quiz_create(&db, atom, draft, Some(&path_id))
        .await
        .unwrap();
    assert_eq!(created, format!("{atom}.u1"));

    answer::cmd_quiz_answer(&db, &created, Rating::Good, None, Some(&path_id))
        .await
        .expect("the graph finds a user quiz by its ID");
    let g = Curriculum::embedded()
        .merged(&common::overlay(&db).await)
        .unwrap();
    let (found, _) = g.quiz(&created).unwrap();
    assert_eq!(found.id, atom);
    assert!(
        g.by_id[atom]
            .quizzes
            .iter()
            .any(|q| q.id == format!("{atom}.q1")),
        "the curriculum quiz stays beside the user quiz"
    );
}

#[tokio::test]
async fn store_on_shipped_lesson_atom_emits_amended() {
    let (db, path_id) = fresh_db_with_path().await;

    store::cmd_lesson_upsert(&db, WITH_LESSON_ATOM, "override".into(), Some(&path_id))
        .await
        .expect("store");

    assert_eq!(
        event_kinds(&db, &path_id).await,
        [amended(WITH_LESSON_ATOM), taught(WITH_LESSON_ATOM)],
    );
}
