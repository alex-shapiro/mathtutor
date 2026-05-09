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
