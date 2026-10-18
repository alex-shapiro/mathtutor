//! Integration tests for the stream-backed event log and the `cards`
//! rows derived from it.
//!
//! Each test opens a fresh database on the shared test server, seeds one
//! path, then logs events through `Changes::append` and reads them back
//! with `event_log::load` and the stored path state. The aim is to pin
//! three contracts:
//!
//! 1. Events round-trip losslessly through the stream (including payload).
//! 2. `QuizAnswered` updates the path's card in the same transaction.
//! 3. `cards::recompute` rebuilds cards deterministically from the event
//!    log alone, matching live append behavior.

use chrono::{Duration, Utc};
use mathtutor::cards::{self, CardRow, CardState};
use mathtutor::event_log::{self, Event, EventKind};
use mathtutor::types::{Rating, Strategy};
use mathtutor::view::Want;
use mathtutor::{Error, db::Db};

mod common;

use common::PATH_ID;

/// Events logged after the seeded `path_created`.
async fn logged(db: &Db, path_id: &str) -> Vec<Event> {
    let events = event_log::load(db, path_id).await.expect("load");
    assert_eq!(events[0].kind, EventKind::PathCreated);
    events[1..].to_vec()
}

fn answered(path: &str, atom: &str, quiz: &str, rating: Rating) -> Event {
    event_log::quiz_answered(path.into(), Some(atom.into()), quiz.into(), rating, None)
}

#[tokio::test]
async fn append_and_load_round_trip() {
    let db = common::fresh_db(PATH_ID).await;
    common::append(
        &db,
        &[
            event_log::lesson_authored(PATH_ID.into(), "atom.x".into()),
            event_log::quiz_presented(PATH_ID.into(), "atom.x".into(), "atom.x.q1".into()),
        ],
    )
    .await;

    let events = logged(&db, PATH_ID).await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, EventKind::LessonAuthored);
    assert_eq!(events[0].atom.as_deref(), Some("atom.x"));
    assert_eq!(events[1].kind, EventKind::QuizPresented);
    assert_eq!(events[1].quiz.as_deref(), Some("atom.x.q1"));
}

#[tokio::test]
async fn load_returns_events_in_insertion_order() {
    // Events with the same `ts` must still come back in append order —
    // stream seqs are what guarantee replayability for FSRS even when
    // wall-clock ticks coincide, within one commit and across commits.
    let db = common::fresh_db(PATH_ID).await;
    let ts = Utc::now();
    common::append(
        &db,
        &[
            common::presented_at("q1", ts),
            common::presented_at("q2", ts),
        ],
    )
    .await;
    common::append(&db, &[common::presented_at("q3", ts)]).await;

    let quizzes: Vec<_> = logged(&db, PATH_ID)
        .await
        .iter()
        .map(|e| e.quiz.clone().unwrap())
        .collect();
    assert_eq!(quizzes, vec!["q1".to_string(), "q2".into(), "q3".into()]);
}

#[tokio::test]
async fn append_preserves_payload_user_answer_and_rating() {
    let db = common::fresh_db(PATH_ID).await;
    common::append(
        &db,
        &[event_log::quiz_answered(
            PATH_ID.into(),
            Some("atom.x".into()),
            "atom.x.q1".into(),
            Rating::Good,
            Some("user said cosine".into()),
        )],
    )
    .await;

    let events = logged(&db, PATH_ID).await;
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.kind, EventKind::QuizAnswered);
    assert_eq!(e.path, PATH_ID);
    assert_eq!(e.atom.as_deref(), Some("atom.x"));
    assert_eq!(e.payload.rating, Some(Rating::Good));
    assert_eq!(e.payload.user_answer.as_deref(), Some("user said cosine"));
}

#[tokio::test]
async fn append_only_touches_the_named_path() {
    // Each path has its own log: events for one path must not leak into
    // another path's view.
    let db = common::fresh_db(PATH_ID).await;
    common::seed_path(&db, "p_other", &[], Strategy::BottomUp).await;

    common::append(
        &db,
        &[event_log::lesson_authored(PATH_ID.into(), "atom.x".into())],
    )
    .await;
    common::append(
        &db,
        &[event_log::lesson_authored(
            "p_other".into(),
            "atom.y".into(),
        )],
    )
    .await;

    let mine = logged(&db, PATH_ID).await;
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].atom.as_deref(), Some("atom.x"));

    let other = logged(&db, "p_other").await;
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].atom.as_deref(), Some("atom.y"));
}

#[tokio::test]
async fn quiz_answered_creates_card() {
    // A single `QuizAnswered` append stores a card whose due date is in
    // the future and whose history reflects the new answer.
    let db = common::fresh_db(PATH_ID).await;
    let before = Utc::now();
    common::append(
        &db,
        &[answered(PATH_ID, "atom.x", "atom.x.q1", Rating::Good)],
    )
    .await;

    let ps = common::path_state(&db, PATH_ID).await;
    let card = ps.cards.get("atom.x.q1").expect("card stored");
    let state = card.state.expect("FSRS state after an answer");
    assert_eq!(card.reps, 1);
    assert_eq!(card.lapses, 0);
    assert_eq!(card.recent, vec![Rating::Good]);
    assert!(card.first_correct_at.is_some());
    assert!(state.due > before, "due should be in the future");
    assert!(state.stability > 0.0);
    assert!(state.difficulty > 0.0);
}

#[tokio::test]
async fn again_rating_increments_lapses() {
    // `lapses` counts only `Again` ratings; a `Hard`/`Good`/`Easy`
    // review must leave it untouched.
    let db = common::fresh_db(PATH_ID).await;
    for rating in [Rating::Again, Rating::Good, Rating::Again] {
        common::append(&db, &[answered(PATH_ID, "atom.x", "atom.x.q1", rating)]).await;
    }

    let ps = common::path_state(&db, PATH_ID).await;
    let card = &ps.cards["atom.x.q1"];
    assert_eq!(card.reps, 3);
    assert_eq!(card.lapses, 2);
    assert_eq!(
        card.recent,
        vec![Rating::Again, Rating::Good, Rating::Again]
    );
}

#[tokio::test]
async fn non_answer_events_leave_cards_unscheduled() {
    // Presenting a quiz records the presentation on its card, but only an
    // answer gives the card FSRS state. Authoring and teaching touch no
    // card at all — otherwise the scheduler would treat unseen quizzes
    // as due-with-default-stability.
    let db = common::fresh_db(PATH_ID).await;
    common::append(
        &db,
        &[
            event_log::quiz_presented(PATH_ID.into(), "atom.x".into(), "atom.x.q1".into()),
            event_log::quiz_authored(PATH_ID.into(), "atom.x".into(), "atom.x.q2".into()),
            event_log::lesson_taught(PATH_ID.into(), "atom.x".into()),
        ],
    )
    .await;

    let ps = common::path_state(&db, PATH_ID).await;
    let card = &ps.cards["atom.x.q1"];
    assert!(
        card.state.is_none(),
        "presented-only card has no FSRS state"
    );
    assert_eq!(card.reps, 0);
    assert_eq!(card.presented, 1);
    assert!(card.last_presented_at.is_some());
    assert!(!ps.cards.contains_key("atom.x.q2"));
    assert_eq!(ps.cards.len(), 1);
    assert!(ps.due(Utc::now() + Duration::days(365)).is_empty());
}

#[tokio::test]
async fn due_returns_only_past_due_in_order() {
    // Cards are stored directly so due dates are controlled without
    // waiting wall-clock time. Only cards due at `now` come back,
    // oldest-due first.
    let db = common::fresh_db(PATH_ID).await;
    let now = Utc::now();
    let rows: [(&str, i64); 3] = [
        ("q.past_old", -3600),  // 1h ago — should be first
        ("q.past_recent", -60), // 1m ago — should be second
        ("q.future", 600),      // 10m from now — must be excluded
    ];
    let cards: Vec<CardRow> = rows
        .iter()
        .map(|(quiz_id, offset_secs)| {
            let due = now + Duration::seconds(*offset_secs);
            CardRow {
                state: Some(CardState {
                    due,
                    last_review: due - Duration::seconds(1),
                    stability: 1.0,
                    difficulty: 5.0,
                }),
                reps: 1,
                ..CardRow::new(PATH_ID, quiz_id)
            }
        })
        .collect();
    common::write(&db, PATH_ID, |_, changes| {
        for c in &cards {
            changes.op(cards::upsert_op(c));
        }
    })
    .await;

    let due = common::path_state(&db, PATH_ID).await.due(now);
    let ids: Vec<&str> = due.iter().map(|(q, _)| q.as_str()).collect();
    assert_eq!(ids, vec!["q.past_old", "q.past_recent"]);
}

#[tokio::test]
async fn recompute_rebuilds_cards_identically_to_live_append() {
    // Append a series of answers, snapshot the resulting card, then call
    // `recompute`. The rebuilt card must match (stability, difficulty,
    // due, reps, lapses, history) — otherwise the recovery path doesn't
    // actually preserve state.
    let db = common::fresh_db(PATH_ID).await;
    let t0 = Utc::now() - Duration::days(60);
    for (rating, ts) in [
        (Rating::Good, t0),
        (Rating::Hard, t0 + Duration::days(10)),
        (Rating::Again, t0 + Duration::days(20)),
        (Rating::Good, t0 + Duration::days(25)),
    ] {
        common::append(&db, &[common::answered_at("atom.x.q1", rating, ts)]).await;
    }

    let before = common::path_state(&db, PATH_ID).await.cards["atom.x.q1"].clone();
    cards::recompute(&db, PATH_ID).await.unwrap();
    let after = common::path_state(&db, PATH_ID).await.cards["atom.x.q1"].clone();

    let (b, a) = (before.state.unwrap(), after.state.unwrap());
    assert!(
        (b.stability - a.stability).abs() < 1e-6,
        "stability {} vs {}",
        b.stability,
        a.stability
    );
    assert!((b.difficulty - a.difficulty).abs() < 1e-6);
    assert_eq!(b.due, a.due);
    assert_eq!(b.last_review, a.last_review);
    assert_eq!(before.reps, after.reps);
    assert_eq!(before.lapses, after.lapses);
    assert_eq!(before.recent, after.recent);
    assert_eq!(before.first_correct_at, after.first_correct_at);
}

#[tokio::test]
async fn failed_transaction_leaves_no_event_or_card() {
    // The event append and the card update commit or fail together: a
    // command that fails after logging an answer leaves neither behind.
    let db = common::fresh_db(PATH_ID).await;
    let r: Result<(), Error> = db
        .transact(&Want::path(Some(PATH_ID)), |view, changes| {
            changes.append(
                view,
                &answered(PATH_ID, "atom.x", "atom.x.q1", Rating::Good),
            )?;
            Err(Error::NoPath)
        })
        .await;
    assert!(matches!(r, Err(Error::NoPath)));

    assert!(logged(&db, PATH_ID).await.is_empty(), "no event committed");
    let ps = common::path_state(&db, PATH_ID).await;
    assert!(ps.cards.is_empty(), "no card committed");
}

#[tokio::test]
async fn recompute_only_touches_target_path() {
    // Recompute must scope its rewrite to the named path. A second
    // path's cards should survive untouched.
    let db = common::fresh_db(PATH_ID).await;
    common::seed_path(&db, "p_other", &[], Strategy::BottomUp).await;
    common::append(
        &db,
        &[answered(PATH_ID, "atom.x", "atom.x.q1", Rating::Good)],
    )
    .await;
    common::append(
        &db,
        &[answered("p_other", "atom.y", "atom.y.q1", Rating::Good)],
    )
    .await;

    cards::recompute(&db, PATH_ID).await.unwrap();

    assert!(
        common::path_state(&db, PATH_ID)
            .await
            .cards
            .contains_key("atom.x.q1")
    );
    assert!(
        common::path_state(&db, "p_other")
            .await
            .cards
            .contains_key("atom.y.q1"),
        "other path's card must survive recompute"
    );
}
