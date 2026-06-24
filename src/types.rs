//! Shared domain enums.
//!
//! Each provides serde for AYML round-trip, `argh::FromArgValue` for CLI
//! parsing, and `Display` where format-string output is needed. Wire
//! formats (`rename_all`) match the AYML and CLI conventions: lowercase
//! for word-shaped variants, `snake_case` for compound ones.

use serde::{Deserialize, Serialize};

use crate::Error;

/// FSRS grade for a quiz answer. The discriminants match the FSRS
/// convention (Again=1, Hard=2, Good=3, Easy=4) and are the stable
/// encoding written to the `events.rating` SQL column.
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
    /// All ratings except `Again` count as a correct answer. `Hard`
    /// means "got it right but with effort"; FSRS still schedules a
    /// sooner re-presentation than `Good`/`Easy`, but the per-atom
    /// walker treats it as correct enough to advance — otherwise a
    /// `Hard` rating would re-surface the quiz immediately, even when
    /// FSRS says wait.
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

/// Quiz difficulty. Every atom has at most one quiz per difficulty.
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

// argh::FromArgValue picks this up via a blanket impl for `T: FromStr
// where T::Err: Display` — no separate CLI parser needed.
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

/// How `mt path next` walks toward a path's targets. A mutable
/// per-path navigation setting, not part of the path's intent.
///
/// - `BottomUp` (default): post-order DFS over prerequisites; never
///   reaches an atom until all its prerequisites are complete.
/// - `TopDown`: presents the next incomplete target directly, descending
///   into prerequisites only via an explicit subpath.
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

// Accepts both the storage form (`bottom_up`) and the friendlier CLI
// spelling (`bottom-up`). `argh::FromArgValue` picks this up via the
// blanket `FromStr` impl.
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

/// Quiz answer mode. Defaults to free-text; multiple-choice is
/// reserved for the rare case where a definition is best taught as a
/// distinguish-this-from-look-alikes.
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
