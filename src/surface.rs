//! Agent interfaces of Math Tutor, the steps that agents send on each interface, and the safe step after a command with an unknown outcome.

/// Interface through which an agent operates Math Tutor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The `mt` command line.
    Cli,
    /// The tools of `mt mcp`.
    Mcp,
}

/// Command or tool call that an agent sends, with a placeholder for each value that the agent supplies.
#[derive(Debug, Clone)]
pub struct Step {
    tool: Tool,
    args: Vec<StepArg>,
}

#[derive(Debug, Clone)]
struct StepArg {
    field: Field,
    value: Value,
    optional: bool,
}

/// Agent command, which is both an MCP tool and an `mt` subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    GetPaths,
    NewPath,
    SetStrategy,
    SubpathSet,
    SubpathClear,
    GetNext,
    GetState,
    GetTree,
    GetSyllabus,
    GetItem,
    GetChildren,
    UpsertLesson,
    CreateQuiz,
    UpdateQuiz,
    DeleteQuiz,
    AnswerQuiz,
}

impl Tool {
    /// Every agent command.
    pub const ALL: &[Tool] = &[
        Tool::GetPaths,
        Tool::NewPath,
        Tool::SetStrategy,
        Tool::SubpathSet,
        Tool::SubpathClear,
        Tool::GetNext,
        Tool::GetState,
        Tool::GetTree,
        Tool::GetSyllabus,
        Tool::GetItem,
        Tool::GetChildren,
        Tool::UpsertLesson,
        Tool::CreateQuiz,
        Tool::UpdateQuiz,
        Tool::DeleteQuiz,
        Tool::AnswerQuiz,
    ];

    /// Return the `mt` subcommand words of this command.
    pub fn cli(self) -> &'static str {
        match self {
            Tool::GetPaths => "path list",
            Tool::NewPath => "path new",
            Tool::SetStrategy => "path strategy",
            Tool::SubpathSet => "path subpath set",
            Tool::SubpathClear => "path subpath clear",
            Tool::GetNext => "path next",
            Tool::GetState => "path state",
            Tool::GetTree => "path tree",
            Tool::GetSyllabus => "path syllabus",
            Tool::GetItem => "graph show",
            Tool::GetChildren => "graph list",
            Tool::UpsertLesson => "lesson upsert",
            Tool::CreateQuiz => "quiz create",
            Tool::UpdateQuiz => "quiz update",
            Tool::DeleteQuiz => "quiz delete",
            Tool::AnswerQuiz => "quiz answer",
        }
    }

    /// Return the MCP tool name of this command.
    pub fn mcp(self) -> &'static str {
        match self {
            Tool::GetPaths => "get_paths",
            Tool::NewPath => "new_path",
            Tool::SetStrategy => "set_strategy",
            Tool::SubpathSet => "subpath_set",
            Tool::SubpathClear => "subpath_clear",
            Tool::GetNext => "get_next",
            Tool::GetState => "get_state",
            Tool::GetTree => "get_tree",
            Tool::GetSyllabus => "get_syllabus",
            Tool::GetItem => "get_item",
            Tool::GetChildren => "get_children",
            Tool::UpsertLesson => "upsert_lesson",
            Tool::CreateQuiz => "create_quiz",
            Tool::UpdateQuiz => "update_quiz",
            Tool::DeleteQuiz => "delete_quiz",
            Tool::AnswerQuiz => "answer_quiz",
        }
    }

    /// Return the operation of this command, which sets the safe step after an unknown outcome.
    pub fn operation(self) -> Operation {
        match self {
            Tool::NewPath => Operation::NewPath,
            Tool::CreateQuiz => Operation::CreateQuiz,
            Tool::AnswerQuiz => Operation::AnswerQuiz,
            Tool::GetPaths
            | Tool::SetStrategy
            | Tool::SubpathSet
            | Tool::SubpathClear
            | Tool::GetNext
            | Tool::GetState
            | Tool::GetTree
            | Tool::GetSyllabus
            | Tool::GetItem
            | Tool::GetChildren
            | Tool::UpsertLesson
            | Tool::UpdateQuiz
            | Tool::DeleteQuiz => Operation::Repeatable,
        }
    }
}

/// Argument of a [`Step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Atom,
    QuizId,
    PathId,
    Body,
    Question,
    Answer,
    Rubric,
    Difficulty,
    QuizType,
    Rating,
    UserAnswer,
}

impl Field {
    /// Return the MCP argument name of this field.
    pub fn mcp(self) -> &'static str {
        match self {
            Field::Atom => "atom",
            Field::QuizId => "quiz_id",
            Field::PathId => "path_id",
            Field::Body => "body",
            Field::Question => "question",
            Field::Answer => "answer",
            Field::UserAnswer => "user_answer",
            Field::Rubric => "rubric",
            Field::Difficulty => "difficulty",
            Field::QuizType => "kind",
            Field::Rating => "rating",
        }
    }

    /// Return the CLI flag of this field, or `None` for the positional argument.
    pub fn cli_flag(self) -> Option<&'static str> {
        match self {
            Field::Atom | Field::QuizId => None,
            Field::PathId => Some("--path"),
            Field::Body => Some("--body"),
            Field::Question => Some("--question"),
            Field::Answer => Some("--answer"),
            Field::Rubric => Some("--rubric"),
            Field::Difficulty => Some("--difficulty"),
            Field::QuizType => Some("--type"),
            Field::Rating => Some("--rating"),
            Field::UserAnswer => Some("--user-answer"),
        }
    }
}

/// Value of a [`Step`] argument.
#[derive(Debug, Clone)]
pub enum Value {
    /// A value that the server knows.
    Known(String),
    /// An identifier that the agent supplies, with its placeholder name.
    Id(&'static str),
    /// Free text that the agent writes, with its placeholder name.
    Text(&'static str),
    /// A value that the agent selects from a fixed list.
    Choice(&'static [&'static str]),
}

impl Step {
    /// Return a step that invokes `tool` on path `path`.
    pub fn new(tool: Tool, path: Value) -> Self {
        Self {
            tool,
            args: vec![StepArg {
                field: Field::PathId,
                value: path,
                optional: false,
            }],
        }
    }

    /// Add the required argument `field`.
    #[must_use]
    pub fn arg(mut self, field: Field, value: Value) -> Self {
        self.args.push(StepArg {
            field,
            value,
            optional: false,
        });
        self
    }

    /// Add the optional argument `field`.
    #[must_use]
    pub fn optional(mut self, field: Field, value: Value) -> Self {
        self.args.push(StepArg {
            field,
            value,
            optional: true,
        });
        self
    }

    /// Return the tool that this step invokes.
    pub fn tool(&self) -> Tool {
        self.tool
    }

    /// Return this step with each placeholder replaced by `value_of` its field, and with every optional argument present.
    #[must_use]
    pub fn fill(&self, value_of: impl Fn(Field) -> String) -> Self {
        let args = self.args.iter().map(|a| StepArg {
            field: a.field,
            value: match &a.value {
                Value::Known(v) => Value::Known(v.clone()),
                _ => Value::Known(value_of(a.field)),
            },
            optional: false,
        });
        Self {
            tool: self.tool,
            args: args.collect(),
        }
    }

    /// Return this step in the syntax of `surface`.
    pub fn render(&self, surface: Surface) -> String {
        match surface {
            Surface::Cli => self.render_cli(),
            Surface::Mcp => self.render_mcp(),
        }
    }

    /// Return the arguments after `mt` that a shell passes for this step.
    pub fn cli_args(&self) -> Vec<String> {
        let mut out: Vec<String> = self.tool.cli().split(' ').map(String::from).collect();
        for a in self.ordered_args() {
            out.extend(a.field.cli_flag().map(String::from));
            out.push(a.value.raw());
        }
        out
    }

    /// Return the MCP arguments of this step as a JSON object.
    pub fn mcp_args(&self) -> serde_json::Map<String, serde_json::Value> {
        self.args
            .iter()
            .map(|a| (a.field.mcp().to_string(), a.value.raw().into()))
            .collect()
    }

    /// Return the arguments with the positional argument first.
    fn ordered_args(&self) -> impl Iterator<Item = &StepArg> {
        let (positional, flags): (Vec<_>, Vec<_>) =
            self.args.iter().partition(|a| a.field.cli_flag().is_none());
        positional.into_iter().chain(flags)
    }

    fn render_cli(&self) -> String {
        let words = self.ordered_args().map(|a| {
            let value = a.value.cli();
            let word = match a.field.cli_flag() {
                Some(flag) => format!("{flag} {value}"),
                None => value,
            };
            if a.optional {
                format!("[{word}]")
            } else {
                word
            }
        });
        std::iter::once(format!("mt {}", self.tool.cli()))
            .chain(words)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn render_mcp(&self) -> String {
        let fields: Vec<String> = self
            .ordered_args()
            .map(|a| {
                let mark = if a.optional { "?" } else { "" };
                format!("{}{mark}: {}", a.field.mcp(), a.value.mcp())
            })
            .collect();
        format!("{} {{ {} }}", self.tool.mcp(), fields.join(", "))
    }
}

impl Value {
    /// Return a value that the server knows.
    pub fn known(v: impl Into<String>) -> Self {
        Value::Known(v.into())
    }

    /// Return the value as an argument string without shell quotes.
    fn raw(&self) -> String {
        match self {
            Value::Known(v) => v.clone(),
            Value::Id(name) | Value::Text(name) => format!("<{name}>"),
            Value::Choice(options) => options.join("|"),
        }
    }

    fn cli(&self) -> String {
        match self {
            Value::Known(v) => shell_word(v),
            Value::Id(name) => format!("<{name}>"),
            Value::Text(name) => format!("'<{name}>'"),
            Value::Choice(options) => format!("<{}>", options.join("|")),
        }
    }

    fn mcp(&self) -> String {
        serde_json::Value::from(self.raw()).to_string()
    }
}

/// Return `s` as a single shell word: unchanged if it holds only safe characters, or in single quotes with each `'` written as `'\''`.
pub fn shell_word(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/:,".contains(&b));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
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
    fn writes_that_must_not_repeat_have_their_own_operation() {
        assert_eq!(Tool::NewPath.operation(), Operation::NewPath);
        assert_eq!(Tool::CreateQuiz.operation(), Operation::CreateQuiz);
        assert_eq!(Tool::AnswerQuiz.operation(), Operation::AnswerQuiz);
        assert_eq!(Tool::DeleteQuiz.operation(), Operation::Repeatable);
        assert_eq!(Tool::GetNext.operation(), Operation::Repeatable);
    }

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
