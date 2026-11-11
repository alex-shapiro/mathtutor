use crate::Result;
use crate::db::Db;
use crate::event_log::{Event, EventKind};

use crate::types::Rating;
use crate::view::ViewRequest;

pub async fn cmd_quiz_answer(
    db: &Db,
    quiz_id: &str,
    rating: Rating,
    user_answer: Option<String>,
    path_id: Option<&str>,
) -> Result<()> {
    db.transact(&ViewRequest::path(path_id), |view, changes| {
        let id = view.path()?.row.id.clone();
        // Verify that the quiz exists in the merged graph before recording an event.
        let g = view.graph()?;
        let (atom, _) = g.quiz(quiz_id)?;
        let answered = EventKind::QuizAnswered {
            atom: Some(atom.id.clone()),
            quiz: quiz_id.to_string(),
            rating,
            user_answer: user_answer.clone(),
        };
        changes.append(view, &Event::now(id, answered))
    })
    .await
}
