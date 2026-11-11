//! Shared domain enums for quiz ratings, difficulties, strategies, and formats.

/// String that is not a spelling of an enum of this module.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {what} '{value}' (expected {expected})", expected = .spellings.join(" | "))]
pub struct InvalidSpelling {
    what: &'static str,
    value: String,
    spellings: &'static [&'static str],
}

/// Declare an enum of unit variants with one spelling each, and derive every string form of the enum from that table.
///
/// The enum gets `ALL`, `SPELLINGS`, `as_str`, `Display`, `FromStr`, serde, and, with the `mcp` feature, a JSON schema that lists the spellings. `argh` parses the enum through `FromStr`.
macro_rules! spelled_enum {
    (
        $(#[$attr:meta])*
        pub enum $name:ident, what: $what:literal {
            $(
                $(#[$variant_attr:meta])*
                $variant:ident = $spelling:literal,
            )+
        }
    ) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($(#[$variant_attr])* $variant,)+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// Spelling of each variant, in declaration order.
            pub const SPELLINGS: &'static [&'static str] = &[$($spelling,)+];

            /// Return the spelling of this variant.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $spelling,)+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = InvalidSpelling;
            fn from_str(s: &str) -> Result<Self, InvalidSpelling> {
                match s {
                    $($spelling => Ok(Self::$variant),)+
                    other => Err(InvalidSpelling {
                        what: $what,
                        value: other.to_string(),
                        spellings: Self::SPELLINGS,
                    }),
                }
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }

        #[cfg(feature = "mcp")]
        impl schemars::JsonSchema for $name {
            fn inline_schema() -> bool {
                true
            }

            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({ "type": "string", "enum": Self::SPELLINGS })
            }
        }
    };
}

spelled_enum! {
    /// FSRS recall rating for a quiz answer.
    pub enum Rating, what: "rating" {
        Again = "again",
        Hard = "hard",
        Good = "good",
        Easy = "easy",
    }
}

impl From<Rating> for i64 {
    /// Return the FSRS grade of `r`, from 1 for `Again` to 4 for `Easy`.
    fn from(r: Rating) -> i64 {
        match r {
            Rating::Again => 1,
            Rating::Hard => 2,
            Rating::Good => 3,
            Rating::Easy => 4,
        }
    }
}

impl Rating {
    /// Return the rating of FSRS grade `grade`, or `None` if no rating has the grade.
    pub fn from_grade(grade: i64) -> Option<Self> {
        Rating::ALL.iter().copied().find(|r| i64::from(*r) == grade)
    }
}

spelled_enum! {
    /// Quiz difficulty level.
    pub enum Difficulty, what: "difficulty" {
        Easy = "easy",
        Medium = "medium",
        Hard = "hard",
    }
}

spelled_enum! {
    /// Learning path traversal strategy.
    #[derive(Default)]
    pub enum Strategy, what: "strategy" {
        #[default]
        BottomUp = "bottom_up",
        TopDown = "top_down",
    }
}

spelled_enum! {
    /// Quiz response format.
    #[derive(Default)]
    pub enum QuizType, what: "quiz type" {
        #[default]
        FreeText = "free_text",
        MultipleChoice = "multiple_choice",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Check that every variant of `T` reads and writes its spelling in every form.
    fn check_spellings<T>(all: &[T], spellings: &[&str])
    where
        T: Copy
            + PartialEq
            + std::fmt::Debug
            + std::fmt::Display
            + std::str::FromStr<Err = InvalidSpelling>
            + serde::Serialize
            + serde::de::DeserializeOwned,
    {
        assert_eq!(all.len(), spellings.len());
        for (value, spelling) in all.iter().zip(spellings) {
            assert_eq!(&value.to_string(), spelling);
            assert_eq!(spelling.parse::<T>().unwrap(), *value);
            assert_eq!(serde_json::to_value(value).unwrap(), *spelling);
            let read: T = serde_json::from_value((*spelling).into()).unwrap();
            assert_eq!(read, *value);
            let parsed: T = argh::FromArgValue::from_arg_value(spelling).unwrap();
            assert_eq!(parsed, *value);
        }
        assert!("unknown".parse::<T>().is_err());
        assert!(serde_json::from_value::<T>("unknown".into()).is_err());
    }

    #[test]
    fn each_enum_has_one_spelling_for_input_and_output() {
        check_spellings(Rating::ALL, Rating::SPELLINGS);
        check_spellings(Difficulty::ALL, Difficulty::SPELLINGS);
        check_spellings(Strategy::ALL, Strategy::SPELLINGS);
        check_spellings(QuizType::ALL, QuizType::SPELLINGS);
        assert!("bottom-up".parse::<Strategy>().is_err());
    }

    #[test]
    fn invalid_value_errors_list_the_spellings() {
        let e = "great".parse::<Rating>().unwrap_err().to_string();
        assert!(e.contains("again | hard | good | easy"), "{e}");
    }

    #[test]
    fn ratings_map_to_fsrs_grades() {
        for (grade, rating) in (1..).zip(Rating::ALL) {
            assert_eq!(i64::from(*rating), grade);
            assert_eq!(Rating::from_grade(grade), Some(*rating));
        }
        assert_eq!(Rating::from_grade(0), None);
    }
}
