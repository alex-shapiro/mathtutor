//! Agent interfaces of Math Tutor and the safe step after a command with an unknown outcome.

/// Interface through which an agent operates Math Tutor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The `mt` command line.
    Cli,
    /// The tools of `mt mcp`.
    Mcp,
}

/// Command kind that sets the safe step after the command fails with an unknown outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    ImportTurso,
    NewPath,
    CreateQuiz,
    AnswerQuiz,
    /// A command that has the same effect if it runs again.
    Repeatable,
}

impl Operation {
    /// Return the safe next step on `surface` after this operation fails with an unknown outcome.
    pub fn unknown_outcome_step(self, surface: Surface) -> &'static str {
        match (self, surface) {
            (Self::ImportTurso, _) => {
                "run `mt import-turso` again; a report that the import already completed means that it succeeded"
            }
            (Self::NewPath, Surface::Cli) => {
                "run `mt path list` and create the path only if it is absent"
            }
            (Self::NewPath, Surface::Mcp) => {
                "call `get_paths` and call `new_path` only if the path is absent"
            }
            (Self::CreateQuiz | Self::AnswerQuiz, Surface::Cli) => {
                "run `mt path next` instead of repeating the command"
            }
            (Self::CreateQuiz | Self::AnswerQuiz, Surface::Mcp) => {
                "call `get_next` instead of repeating the call"
            }
            (Self::Repeatable, Surface::Cli) => "repeat the command",
            (Self::Repeatable, Surface::Mcp) => "repeat the call",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_after_a_write_that_must_not_repeat_advance_instead() {
        for op in [Operation::CreateQuiz, Operation::AnswerQuiz] {
            assert!(
                op.unknown_outcome_step(Surface::Cli)
                    .contains("`mt path next`")
            );
            assert!(op.unknown_outcome_step(Surface::Mcp).contains("`get_next`"));
        }
    }

    #[test]
    fn mcp_steps_name_only_mcp_tools() {
        for op in [
            Operation::NewPath,
            Operation::CreateQuiz,
            Operation::AnswerQuiz,
            Operation::Repeatable,
        ] {
            assert!(!op.unknown_outcome_step(Surface::Mcp).contains("mt "));
        }
    }
}
