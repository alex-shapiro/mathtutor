//! Shared domain enums for quiz ratings, difficulties, strategies, and formats.

/// Error returned if a string does not match any variant of an enum in this module.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {what} '{value}' (expected {expected})", expected = .names.join(" | "))]
pub struct InvalidVariantName {
    what: &'static str,
    value: String,
    names: &'static [&'static str],
}

/// Declare an enum of unit variants with a string name per variant.
///
/// Generates `ALL`, `NAMES`, `as_str`, `Display`, `FromStr`, serde traits, and a `schemars::JsonSchema` implementation under the `mcp` feature.
macro_rules! named_enum {
    (
        $(#[$attr:meta])*
        pub enum $name:ident, what: $what:literal {
            $(
                $(#[$variant_attr:meta])*
                $variant:ident = $variant_name:literal,
            )+
        }
    ) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($(#[$variant_attr])* $variant,)+
        }

        impl $name {
            /// Every variant in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// Name of each variant in declaration order.
            pub const NAMES: &'static [&'static str] = &[$($variant_name,)+];

            /// Return the string name of this variant.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $variant_name,)+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = InvalidVariantName;
            fn from_str(s: &str) -> Result<Self, InvalidVariantName> {
                match s {
                    $($variant_name => Ok(Self::$variant),)+
                    other => Err(InvalidVariantName {
                        what: $what,
                        value: other.to_string(),
                        names: Self::NAMES,
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
                schemars::json_schema!({ "type": "string", "enum": Self::NAMES })
            }
        }
    };
}

named_enum! {
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
    /// Return the rating for FSRS grade `grade`, or `None` if `grade` is out of range.
    pub fn from_grade(grade: i64) -> Option<Self> {
        Rating::ALL.iter().copied().find(|r| i64::from(*r) == grade)
    }
}

named_enum! {
    /// Quiz difficulty level.
    pub enum Difficulty, what: "difficulty" {
        Easy = "easy",
        Medium = "medium",
        Hard = "hard",
    }
}

named_enum! {
    /// Learning path traversal strategy.
    #[derive(Default)]
    pub enum Strategy, what: "strategy" {
        #[default]
        BottomUp = "bottom_up",
        TopDown = "top_down",
    }
}

named_enum! {
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

    /// Check that every variant of `T` round-trips through its string name.
    fn check_variant_names<T>(all: &[T], names: &[&str])
    where
        T: Copy
            + PartialEq
            + std::fmt::Debug
            + std::fmt::Display
            + std::str::FromStr<Err = InvalidVariantName>
            + serde::Serialize
            + serde::de::DeserializeOwned,
    {
        assert_eq!(all.len(), names.len());
        for (value, name) in all.iter().zip(names) {
            assert_eq!(&value.to_string(), name);
            assert_eq!(name.parse::<T>().unwrap(), *value);
            assert_eq!(serde_json::to_value(value).unwrap(), *name);
            let read: T = serde_json::from_value((*name).into()).unwrap();
            assert_eq!(read, *value);
            let parsed: T = argh::FromArgValue::from_arg_value(name).unwrap();
            assert_eq!(parsed, *value);
        }
        assert!("unknown".parse::<T>().is_err());
        assert!(serde_json::from_value::<T>("unknown".into()).is_err());
    }

    #[test]
    fn each_enum_uses_matching_names_for_input_and_output() {
        check_variant_names(Rating::ALL, Rating::NAMES);
        check_variant_names(Difficulty::ALL, Difficulty::NAMES);
        check_variant_names(Strategy::ALL, Strategy::NAMES);
        check_variant_names(QuizType::ALL, QuizType::NAMES);
        assert!("bottom-up".parse::<Strategy>().is_err());
    }

    #[test]
    fn invalid_value_errors_list_the_variant_names() {
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
