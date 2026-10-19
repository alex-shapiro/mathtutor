//! Shared domain enums for quiz ratings, difficulties, strategies, and formats.

use serde::{Deserialize, Serialize};

use crate::Error;

/// FSRS recall rating for a quiz answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema), schemars(inline))]
#[serde(rename_all = "lowercase")]
#[repr(i64)]
pub enum Rating {
    Again = 1,
    Hard = 2,
    Good = 3,
    Easy = 4,
}

impl Rating {
    /// Return `true` if the rating is `Hard`, `Good`, or `Easy`.
    pub fn is_correct(self) -> bool {
        !matches!(self, Rating::Again)
    }
}

impl From<Rating> for i64 {
    fn from(r: Rating) -> i64 {
        r as i64
    }
}

impl TryFrom<i64> for Rating {
    type Error = Error;
    fn try_from(v: i64) -> Result<Self, Error> {
        match v {
            1 => Ok(Rating::Again),
            2 => Ok(Rating::Hard),
            3 => Ok(Rating::Good),
            4 => Ok(Rating::Easy),
            other => Err(Error::InvalidRating(other)),
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

/// Quiz difficulty level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema), schemars(inline))]
#[serde(rename_all = "lowercase")]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
}

impl Difficulty {
    pub fn as_str(self) -> &'static str {
        match self {
            Difficulty::Easy => "easy",
            Difficulty::Medium => "medium",
            Difficulty::Hard => "hard",
        }
    }
}

impl std::fmt::Display for Difficulty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Difficulty {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        match s {
            "easy" => Ok(Difficulty::Easy),
            "medium" => Ok(Difficulty::Medium),
            "hard" => Ok(Difficulty::Hard),
            other => Err(Error::InvalidDifficulty(other.to_string())),
        }
    }
}

/// Learning path traversal strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema), schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    #[default]
    BottomUp,
    TopDown,
}

impl Strategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::BottomUp => "bottom_up",
            Strategy::TopDown => "top_down",
        }
    }
}

impl std::fmt::Display for Strategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Strategy {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        match s {
            "bottom_up" | "bottom-up" => Ok(Strategy::BottomUp),
            "top_down" | "top-down" => Ok(Strategy::TopDown),
            other => Err(Error::InvalidStrategy(other.to_string())),
        }
    }
}

/// Quiz response format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema), schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum QuizType {
    #[default]
    FreeText,
    MultipleChoice,
}

impl QuizType {
    pub fn as_str(self) -> &'static str {
        match self {
            QuizType::FreeText => "free_text",
            QuizType::MultipleChoice => "multiple_choice",
        }
    }
}

impl std::str::FromStr for QuizType {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        match s {
            "free_text" => Ok(QuizType::FreeText),
            "multiple_choice" => Ok(QuizType::MultipleChoice),
            other => Err(Error::InvalidQuizType(other.to_string())),
        }
    }
}
