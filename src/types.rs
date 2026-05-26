//! Shared domain enums.
//!
//! Each provides serde for AYML round-trip, `argh::FromArgValue` for CLI
//! parsing, and `Display` where format-string output is needed. Wire
//! formats (`rename_all`) match the AYML and CLI conventions: lowercase
//! for word-shaped variants, `snake_case` for compound ones.

use serde::{Deserialize, Serialize};

/// FSRS grade for a quiz answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rating {
    Again,
    Hard,
    Good,
    Easy,
}

impl Rating {
    /// All ratings except `Again` count as a correct answer. `Hard`
    /// means "got it right but with effort"; FSRS still schedules a
    /// sooner re-presentation than `Good`/`Easy`, but the per-atom
    /// walker treats it as correct enough to advance — otherwise a
    /// `Hard` rating would re-surface the quiz immediately, even when
    /// FSRS says wait.
    pub fn is_correct(self) -> bool {
        !matches!(self, Rating::Again)
    }

    /// Stable integer encoding for the `events.rating` SQL column.
    /// Matches the FSRS convention (Again=1, Hard=2, Good=3, Easy=4).
    pub fn as_int(self) -> i64 {
        match self {
            Rating::Again => 1,
            Rating::Hard => 2,
            Rating::Good => 3,
            Rating::Easy => 4,
        }
    }

    /// Inverse of `as_int`; returns `None` for any out-of-range value.
    pub fn from_int(v: i64) -> Option<Self> {
        match v {
            1 => Some(Rating::Again),
            2 => Some(Rating::Hard),
            3 => Some(Rating::Good),
            4 => Some(Rating::Easy),
            _ => None,
        }
    }
}

impl std::fmt::Display for Rating {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Rating::Again => "again",
            Rating::Hard => "hard",
            Rating::Good => "good",
            Rating::Easy => "easy",
        })
    }
}

impl argh::FromArgValue for Rating {
    fn from_arg_value(value: &str) -> Result<Self, String> {
        match value {
            "again" => Ok(Rating::Again),
            "hard" => Ok(Rating::Hard),
            "good" => Ok(Rating::Good),
            "easy" => Ok(Rating::Easy),
            other => Err(format!(
                "invalid rating '{other}' (expected again | hard | good | easy)"
            )),
        }
    }
}

/// Quiz difficulty slot — every atom has at most one quiz per difficulty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
}

impl std::fmt::Display for Difficulty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Difficulty::Easy => "easy",
            Difficulty::Medium => "medium",
            Difficulty::Hard => "hard",
        })
    }
}

impl argh::FromArgValue for Difficulty {
    fn from_arg_value(value: &str) -> Result<Self, String> {
        match value {
            "easy" => Ok(Difficulty::Easy),
            "medium" => Ok(Difficulty::Medium),
            "hard" => Ok(Difficulty::Hard),
            other => Err(format!(
                "invalid difficulty '{other}' (expected easy | medium | hard)"
            )),
        }
    }
}

/// Quiz answer mode. Defaults to free-text; multiple-choice is
/// reserved for the rare case where a definition is best taught as a
/// distinguish-this-from-look-alikes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QuizType {
    #[default]
    FreeText,
    MultipleChoice,
}

impl argh::FromArgValue for QuizType {
    fn from_arg_value(value: &str) -> Result<Self, String> {
        match value {
            "free_text" => Ok(QuizType::FreeText),
            "multiple_choice" => Ok(QuizType::MultipleChoice),
            other => Err(format!(
                "invalid quiz type '{other}' (expected free_text | multiple_choice)"
            )),
        }
    }
}
