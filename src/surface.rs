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
    tool: AgentCommand,
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
pub enum AgentCommand {
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

impl AgentCommand {
    /// Every agent command.
    pub const ALL: &[AgentCommand] = &[
        AgentCommand::GetPaths,
        AgentCommand::NewPath,
        AgentCommand::SetStrategy,
        AgentCommand::SubpathSet,
        AgentCommand::SubpathClear,
        AgentCommand::GetNext,
        AgentCommand::GetState,
        AgentCommand::GetTree,
        AgentCommand::GetSyllabus,
        AgentCommand::GetItem,
        AgentCommand::GetChildren,
        AgentCommand::UpsertLesson,
        AgentCommand::CreateQuiz,
        AgentCommand::UpdateQuiz,
        AgentCommand::DeleteQuiz,
        AgentCommand::AnswerQuiz,
    ];

    /// Return the `mt` subcommand words of this command.
    pub fn cli(self) -> &'static str {
        match self {
            AgentCommand::GetPaths => "path list",
            AgentCommand::NewPath => "path new",
            AgentCommand::SetStrategy => "path strategy",
            AgentCommand::SubpathSet => "path subpath set",
            AgentCommand::SubpathClear => "path subpath clear",
            AgentCommand::GetNext => "path next",
            AgentCommand::GetState => "path state",
            AgentCommand::GetTree => "path tree",
            AgentCommand::GetSyllabus => "path syllabus",
            AgentCommand::GetItem => "graph show",
            AgentCommand::GetChildren => "graph list",
            AgentCommand::UpsertLesson => "lesson upsert",
            AgentCommand::CreateQuiz => "quiz create",
            AgentCommand::UpdateQuiz => "quiz update",
            AgentCommand::DeleteQuiz => "quiz delete",
            AgentCommand::AnswerQuiz => "quiz answer",
        }
    }

    /// Return the MCP tool name of this command.
    pub fn mcp(self) -> &'static str {
        match self {
            AgentCommand::GetPaths => "get_paths",
            AgentCommand::NewPath => "new_path",
            AgentCommand::SetStrategy => "set_strategy",
            AgentCommand::SubpathSet => "subpath_set",
            AgentCommand::SubpathClear => "subpath_clear",
            AgentCommand::GetNext => "get_next",
            AgentCommand::GetState => "get_state",
            AgentCommand::GetTree => "get_tree",
            AgentCommand::GetSyllabus => "get_syllabus",
            AgentCommand::GetItem => "get_item",
            AgentCommand::GetChildren => "get_children",
            AgentCommand::UpsertLesson => "upsert_lesson",
            AgentCommand::CreateQuiz => "create_quiz",
            AgentCommand::UpdateQuiz => "update_quiz",
            AgentCommand::DeleteQuiz => "delete_quiz",
            AgentCommand::AnswerQuiz => "answer_quiz",
        }
    }

    /// Return the safe next step after this command fails with an unknown outcome.
    pub fn recovery(self) -> Recovery {
        match self {
            AgentCommand::NewPath => Recovery::ListPathsFirst,
            AgentCommand::CreateQuiz | AgentCommand::AnswerQuiz => Recovery::GetNext,
            AgentCommand::GetPaths
            | AgentCommand::SetStrategy
            | AgentCommand::SubpathSet
            | AgentCommand::SubpathClear
            | AgentCommand::GetNext
            | AgentCommand::GetState
            | AgentCommand::GetTree
            | AgentCommand::GetSyllabus
            | AgentCommand::GetItem
            | AgentCommand::GetChildren
            | AgentCommand::UpsertLesson
            | AgentCommand::UpdateQuiz
            | AgentCommand::DeleteQuiz => Recovery::Repeat,
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
    pub fn new(tool: AgentCommand, path: Value) -> Self {
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

    /// Return the agent command that this step invokes.
    pub fn agent_command(&self) -> AgentCommand {
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

/// Safe next step after a command fails with an unknown outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Run the import again, which reports a completed import.
    RunImportAgain,
    /// List the paths, and create the path only if it is absent.
    ListPathsFirst,
    /// Ask for the next action, which shows whether the write applied, instead of a repeat that could apply it twice.
    GetNext,
    /// Repeat the command, which has the same effect if it runs again.
    Repeat,
}

impl Recovery {
    /// Return the text of this step on `surface`.
    pub fn step(self, surface: Surface) -> &'static str {
        match (self, surface) {
            (Self::RunImportAgain, _) => {
                "run `mt import-turso` again; a report that the import already completed means that it succeeded"
            }
            (Self::ListPathsFirst, Surface::Cli) => {
                "run `mt path list` and create the path only if it is absent"
            }
            (Self::ListPathsFirst, Surface::Mcp) => {
                "call `get_paths` and call `new_path` only if the path is absent"
            }
            (Self::GetNext, Surface::Cli) => "run `mt path next` instead of repeating the command",
            (Self::GetNext, Surface::Mcp) => "call `get_next` instead of repeating the call",
            (Self::Repeat, Surface::Cli) => "repeat the command",
            (Self::Repeat, Surface::Mcp) => "repeat the call",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_that_must_not_repeat_have_their_own_recovery() {
        assert_eq!(AgentCommand::NewPath.recovery(), Recovery::ListPathsFirst);
        assert_eq!(AgentCommand::CreateQuiz.recovery(), Recovery::GetNext);
        assert_eq!(AgentCommand::AnswerQuiz.recovery(), Recovery::GetNext);
        assert_eq!(AgentCommand::DeleteQuiz.recovery(), Recovery::Repeat);
        assert_eq!(AgentCommand::GetNext.recovery(), Recovery::Repeat);
    }

    #[test]
    fn steps_after_a_write_that_must_not_repeat_advance_instead() {
        assert!(
            Recovery::GetNext
                .step(Surface::Cli)
                .contains("`mt path next`")
        );
        assert!(Recovery::GetNext.step(Surface::Mcp).contains("`get_next`"));
    }

    #[test]
    fn mcp_steps_name_only_mcp_tools() {
        for recovery in [
            Recovery::ListPathsFirst,
            Recovery::GetNext,
            Recovery::Repeat,
        ] {
            assert!(!recovery.step(Surface::Mcp).contains("mt "));
        }
    }
}
