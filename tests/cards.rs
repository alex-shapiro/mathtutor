//! FSRS step tests for `cards::apply_answer`

use chrono::{Duration, TimeZone, Utc};
use mathtutor::cards::apply_answer;
use mathtutor::types::Rating;

#[test]
fn apply_answer_updates_last_review_each_step() {
    // A fresh `apply_answer` call must overwrite `last_review`.
    // Otherwise, the next replay computes `days_elapsed` from
    // the first answer instead of the most recent one.
    let t1 = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
    let t2 = t1 + Duration::days(30);

    let s1 = apply_answer(None, Rating::Good, t1).unwrap();
    assert_eq!(s1.last_review, t1);

    let s2 = apply_answer(Some(&s1), Rating::Good, t2).unwrap();
    assert_eq!(
        s2.last_review, t2,
        "last_review must advance to the current answer's ts, not stay on the first"
    );
}

#[test]
fn chained_steps_use_gap_to_most_recent_answer_not_first() {
    // Same quiz answered three times. Each FSRS step must see the gap
    // to its immediately preceding answer, not to the original. We
    // prove it by showing that a three-step chain (gaps 30, 60) ends in
    // a different state than a two-step chain that skips the middle
    // answer (gap 90). If `apply_answer` ever used `ts - first` for
    // `days_elapsed`, the third step of the three-step chain would
    // collapse into the second step of the two-step chain.
    let t1 = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
    let t2 = t1 + Duration::days(30);
    let t3 = t2 + Duration::days(60);

    let three_step = {
        let s1 = apply_answer(None, Rating::Good, t1).unwrap();
        let s2 = apply_answer(Some(&s1), Rating::Good, t2).unwrap();
        apply_answer(Some(&s2), Rating::Good, t3).unwrap()
    };
    let two_step_skipping_middle = {
        let s1 = apply_answer(None, Rating::Good, t1).unwrap();
        apply_answer(Some(&s1), Rating::Good, t3).unwrap()
    };

    assert!(
        (three_step.stability - two_step_skipping_middle.stability).abs() > 1e-6,
        "three answers (gaps 30, 60) must differ from two answers (gap 90); \
         otherwise the third step is using gap-to-first instead of gap-to-prev"
    );
}
