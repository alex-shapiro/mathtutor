//! Integration tests for the event log and derived card state.

use chrono::{DateTime, Duration, Utc};
use mathtutor::cards::{CardRow, CardState};
use mathtutor::event_log::{self, Event, EventKind};
use mathtutor::types::{Rating, Strategy};
use mathtutor::view::Want;
use mathtutor::{Error, db::Db};

mod common;

use common::PATH_ID;

async fn logged(db: &Db, path_id: &str) -> Vec<Event> {
    let events = event_log::load(db, path_id).await.expect("load");
    assert_eq!(events[0].kind, EventKind::PathCreated {});
    events[1..].to_vec()
}

fn answered(path: &str, atom: &str, quiz: &str, rating: Rating) -> Event {
    Event::now(
        path,
        EventKind::QuizAnswered {
            atom: Some(atom.into()),
            quiz: quiz.into(),
            rating,
            user_answer: None,
        },
    )
}

#[tokio::test]
async fn append_and_load_round_trip() {
    let db = common::fresh_db(PATH_ID).await;
    common::append(
        &db,
        &[
            Event::now(
                PATH_ID,
                EventKind::LessonAuthored {
                    atom: "atom.x".into(),
                },
            ),
            Event::now(
                PATH_ID,
                EventKind::QuizPresented {
                    atom: "atom.x".into(),
                    quiz: "atom.x.q1".into(),
                },
            ),
        ],
    )
    .await;

    let kinds: Vec<EventKind> = logged(&db, PATH_ID)
        .await
        .into_iter()
        .map(|e| e.kind)
        .collect();
    let authored = EventKind::LessonAuthored {
        atom: "atom.x".into(),
    };
    let presented = EventKind::QuizPresented {
        atom: "atom.x".into(),
        quiz: "atom.x.q1".into(),
    };
    assert_eq!(kinds, [authored, presented]);
}

#[tokio::test]
async fn load_returns_events_in_insertion_order() {
    let db = common::fresh_db(PATH_ID).await;
    let ts = Utc::now();
    common::append(
        &db,
        &[
            common::at(ts, common::presented("q1")),
            common::at(ts, common::presented("q2")),
        ],
    )
    .await;
    common::append(&db, &[common::at(ts, common::presented("q3"))]).await;

    let quizzes: Vec<_> = logged(&db, PATH_ID)
        .await
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::QuizPresented { quiz, .. } => Some(quiz.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(quizzes, vec!["q1".to_string(), "q2".into(), "q3".into()]);
}

#[tokio::test]
async fn append_preserves_payload_user_answer_and_rating() {
    let db = common::fresh_db(PATH_ID).await;
    let answer = Event::now(
        PATH_ID,
        EventKind::QuizAnswered {
            atom: Some("atom.x".into()),
            quiz: "atom.x.q1".into(),
            rating: Rating::Good,
            user_answer: Some("user said cosine".into()),
        },
    );
    common::append(&db, std::slice::from_ref(&answer)).await;

    let events = logged(&db, PATH_ID).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, answer.kind);
    assert_eq!(events[0].path, PATH_ID);
}

#[tokio::test]
async fn append_only_touches_the_named_path() {
    let db = common::fresh_db(PATH_ID).await;
    common::seed_path(&db, "p_other", &[], Strategy::BottomUp).await;

    common::append(
        &db,
        &[Event::now(
            PATH_ID,
            EventKind::LessonAuthored {
                atom: "atom.x".into(),
            },
        )],
    )
    .await;
    common::append(
        &db,
        &[Event::now(
            "p_other",
            EventKind::LessonAuthored {
                atom: "atom.y".into(),
            },
        )],
    )
    .await;

    let authored = |atom: &str| EventKind::LessonAuthored { atom: atom.into() };
    let mine = logged(&db, PATH_ID).await;
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].kind, authored("atom.x"));

    let other = logged(&db, "p_other").await;
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].kind, authored("atom.y"));
}

#[tokio::test]
async fn quiz_answered_creates_card() {
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
    let db = common::fresh_db(PATH_ID).await;
    common::append(
        &db,
        &[
            Event::now(
                PATH_ID,
                EventKind::QuizPresented {
                    atom: "atom.x".into(),
                    quiz: "atom.x.q1".into(),
                },
            ),
            Event::now(
                PATH_ID,
                EventKind::QuizAuthored {
                    atom: "atom.x".into(),
                    quiz: "atom.x.q2".into(),
                },
            ),
            Event::now(
                PATH_ID,
                EventKind::LessonTaught {
                    atom: "atom.x".into(),
                },
            ),
        ],
    )
    .await;

    let ps = common::path_state(&db, PATH_ID).await;
    let card = &ps.cards["atom.x.q1"];
    assert!(card.state.is_none(), "presented-only card lacks FSRS state");
    assert_eq!(card.reps, 0);
    assert_eq!(card.presented, 1);
    assert!(card.last_presented_at.is_some());
    assert!(!ps.cards.contains_key("atom.x.q2"));
    assert_eq!(ps.cards.len(), 1);
    assert_eq!(
        ps.due(Utc::now() + Duration::days(365)),
        Vec::<(String, DateTime<Utc>)>::new()
    );
}

#[tokio::test]
async fn due_returns_only_past_due_in_order() {
    let db = common::fresh_db(PATH_ID).await;
    let now = Utc::now();
    let rows: [(&str, i64); 3] = [
        ("q.past_old", -3600),
        ("q.past_recent", -60),
        ("q.future", 600),
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
            changes.set_card(c.clone());
        }
    })
    .await;

    let due = common::path_state(&db, PATH_ID).await.due(now);
    let ids: Vec<&str> = due.iter().map(|(q, _)| q.as_str()).collect();
    assert_eq!(ids, vec!["q.past_old", "q.past_recent"]);
}

#[tokio::test]
async fn failed_transaction_does_not_write_events_or_cards() {
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

    assert_eq!(logged(&db, PATH_ID).await, []);
    let ps = common::path_state(&db, PATH_ID).await;
    assert!(ps.cards.is_empty());
}
