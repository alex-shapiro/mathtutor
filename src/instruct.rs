//! Agent playbook for each surface, rendered from one template.

use crate::Result;
use crate::surface::{AgentCommand, Field, Step, Surface, Value};
use crate::types::{Difficulty, QuizType, Rating};

/// Playbook template.
///
/// A line `<!-- cli -->` or `<!-- mcp -->` starts a block for that surface only, and `<!-- end -->` ends it. Each `{{name}}` is a term of [`TERMS`], [`TOOL_TERMS`], or [`FIELD_TERMS`], or a step of [`playbook_steps`].
const TEMPLATE: &str = include_str!("playbook.md");

/// Name, CLI form, and MCP form of each term in [`TEMPLATE`].
const TERMS: &[(&str, &str, &str)] = &[
    ("surface", "the `mt` CLI", "the Math Tutor MCP server"),
    ("server", "`mt`", "The MCP server"),
    ("step", "command", "tool call"),
    (
        "set_strategy",
        "Pass `--strategy top_down` to `mt path new`, or change the strategy at any time with `mt path strategy <bottom_up|top_down>`",
        "Pass `strategy` to `new_path`, or change the strategy at any time with `set_strategy { path_id, strategy }`",
    ),
    (
        "get_next",
        "Run `mt path next`",
        "Call `get_next { path_id }`",
    ),
    (
        "get_next_again",
        "Continue with `mt path next`",
        "Call `get_next` again",
    ),
    (
        "get_due",
        "run `mt path next --due`",
        "call `get_next { path_id, mode: \"due\" }`",
    ),
    (
        "inspect_target",
        "Run `mt graph show <target>`",
        "Call `get_item { id: <target> }`",
    ),
    (
        "subpath_set_again",
        "run `mt path subpath set` again",
        "call `subpath_set` again",
    ),
    ("run", "Run", "Call"),
    (
        "error_report",
        "`mt` prints errors to stderr and exits with a non-zero status code.",
        "Tool errors return `isError: true` with a JSON error object.",
    ),
    (
        "unknown_outcome",
        "Exit code 3 indicates that the commit status is unknown, and `mt` then prints the safe next step on stderr.",
        "The error kind `outcome_unknown` indicates that the commit status is unknown, and the `next_step` field of the error then gives the safe next call.",
    ),
];

/// Name and command of each term in [`TEMPLATE`] that names an agent command.
const TOOL_TERMS: &[(&str, AgentCommand)] = &[
    ("next_name", AgentCommand::GetNext),
    ("answer_name", AgentCommand::AnswerQuiz),
    ("create_quiz_name", AgentCommand::CreateQuiz),
    ("update_quiz_name", AgentCommand::UpdateQuiz),
    ("delete_quiz_name", AgentCommand::DeleteQuiz),
];

/// Name and field of each term in [`TEMPLATE`] that names a command argument.
const FIELD_TERMS: &[(&str, Field)] = &[
    ("quiz_type", Field::QuizType),
    ("verbatim_field", Field::UserAnswer),
];

/// Return the steps that the playbook names, each with its term name.
pub fn playbook_steps() -> Vec<(&'static str, Step)> {
    let path = || Value::Id("path-id");
    let atom = || Value::Id("atom-id");
    let quiz = || Value::Id("quiz-id");
    vec![
        (
            "upsert_lesson",
            Step::new(AgentCommand::UpsertLesson, path())
                .arg(Field::Atom, atom())
                .arg(Field::Body, Value::Text("lesson")),
        ),
        (
            "create_quiz",
            Step::new(AgentCommand::CreateQuiz, path())
                .arg(Field::Atom, atom())
                .arg(Field::Difficulty, Value::Choice(Difficulty::SPELLINGS))
                .arg(Field::Question, Value::Text("question"))
                .arg(Field::Answer, Value::Text("answer"))
                .optional(Field::Rubric, Value::Text("rubric"))
                .optional(Field::QuizType, Value::Choice(QuizType::SPELLINGS)),
        ),
        (
            "answer_quiz",
            Step::new(AgentCommand::AnswerQuiz, path())
                .arg(Field::QuizId, quiz())
                .arg(Field::Rating, Value::Choice(Rating::SPELLINGS))
                .arg(Field::UserAnswer, Value::Text("verbatim reply")),
        ),
        (
            "update_quiz",
            Step::new(AgentCommand::UpdateQuiz, path())
                .arg(Field::QuizId, quiz())
                .optional(Field::Question, Value::Text("question"))
                .optional(Field::Answer, Value::Text("answer"))
                .optional(Field::Rubric, Value::Text("rubric"))
                .optional(Field::Difficulty, Value::Choice(Difficulty::SPELLINGS))
                .optional(Field::QuizType, Value::Choice(QuizType::SPELLINGS)),
        ),
        (
            "delete_quiz",
            Step::new(AgentCommand::DeleteQuiz, path()).arg(Field::QuizId, quiz()),
        ),
        (
            "subpath_set",
            Step::new(AgentCommand::SubpathSet, path())
                .arg(Field::Atoms, Value::IdList(&["prereq", "...", "target"])),
        ),
        (
            "subpath_clear",
            Step::new(AgentCommand::SubpathClear, path()),
        ),
    ]
}

/// Return the agent playbook for `surface`.
pub fn playbook(surface: Surface) -> String {
    let mut out = String::with_capacity(TEMPLATE.len());
    let mut block: Option<Surface> = None;
    for line in TEMPLATE.lines() {
        match line.trim() {
            "<!-- cli -->" => block = Some(Surface::Cli),
            "<!-- mcp -->" => block = Some(Surface::Mcp),
            "<!-- end -->" => block = None,
            _ if block.is_none_or(|b| b == surface) => {
                out.push_str(line);
                out.push('\n');
            }
            _ => {}
        }
    }
    for (name, cli, mcp) in TERMS {
        let term = match surface {
            Surface::Cli => cli,
            Surface::Mcp => mcp,
        };
        out = out.replace(&format!("{{{{{name}}}}}"), term);
    }
    for (name, tool) in TOOL_TERMS {
        let term = match surface {
            Surface::Cli => format!("`mt {}`", tool.cli()),
            Surface::Mcp => format!("`{}`", tool.mcp()),
        };
        out = out.replace(&format!("{{{{{name}}}}}"), &term);
    }
    for (name, field) in FIELD_TERMS {
        let term = match surface {
            Surface::Cli => field.cli_flag(),
            Surface::Mcp => Some(field.mcp()),
        };
        if let Some(term) = term {
            out = out.replace(&format!("{{{{{name}}}}}"), &format!("`{term}`"));
        }
    }
    for (name, step) in playbook_steps() {
        out = out.replace(&format!("{{{{{name}}}}}"), &step.render(surface));
    }
    out
}

/// Return every playbook step and every scheduled step with valid values, once for each option of each choice.
#[cfg(test)]
pub(crate) fn filled_steps() -> Vec<Step> {
    use crate::scheduler::{Action, next_step};
    use crate::types::Difficulty;

    let atom_id = || "fnd.1.1.1".to_string();
    let actions = [
        Action::CreateLesson { atom_id: atom_id() },
        Action::PresentLesson { atom_id: atom_id() },
        Action::CreateQuiz {
            atom_id: atom_id(),
            difficulty: Difficulty::Hard,
        },
        Action::PresentQuiz {
            quiz_id: "fnd.1.1.1.q1".into(),
            atom_id: atom_id(),
        },
        Action::NoReviewsDue,
        Action::Done,
    ];
    let steps: Vec<Step> = playbook_steps()
        .into_iter()
        .map(|(_, step)| step)
        .chain(actions.iter().filter_map(|a| next_step(a, "p_test")))
        .collect();
    (0..Rating::SPELLINGS.len())
        .flat_map(|i| {
            steps.iter().map(move |step| {
                step.fill(|field| match field {
                    Field::Atom | Field::Atoms => atom_id(),
                    Field::QuizId => "fnd.1.1.1.q1".into(),
                    Field::PathId => "p_test".into(),
                    Field::Difficulty => {
                        Difficulty::SPELLINGS[i % Difficulty::SPELLINGS.len()].into()
                    }
                    Field::QuizType => QuizType::SPELLINGS[i % QuizType::SPELLINGS.len()].into(),
                    Field::Rating => Rating::SPELLINGS[i].into(),
                    Field::Body
                    | Field::Question
                    | Field::Answer
                    | Field::Rubric
                    | Field::UserAnswer => "text".into(),
                })
            })
        })
        .collect()
}

/// Print the CLI agent playbook (`mt instruct`).
pub fn cmd_instruct() -> Result<()> {
    crate::print_stdout(&playbook(Surface::Cli))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use argh::FromArgs;

    use super::*;
    use crate::cli::Mt;

    /// Return the inline code spans of `text`, and the code lines that start with 4 spaces.
    fn code_spans(text: &str) -> Vec<&str> {
        let mut out = Vec::new();
        for line in text.lines() {
            out.extend(line.split('`').skip(1).step_by(2));
            out.extend(line.strip_prefix("    "));
        }
        out
    }

    /// Return the subcommand words and the flags of each `mt` command that `text` names in code.
    fn cli_commands(text: &str) -> Vec<(Vec<&str>, Vec<&str>)> {
        let is_word = |w: &str| {
            w.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && w.chars().all(|c| c.is_ascii_lowercase() || c == '-')
        };
        code_spans(text)
            .into_iter()
            .filter_map(|span| span.strip_prefix("mt "))
            .map(|command| {
                let tokens: Vec<&str> = command.split(' ').collect();
                let n = tokens.iter().take_while(|w| is_word(w)).count();
                let flags = tokens[n..]
                    .iter()
                    .map(|t| t.trim_matches(['[', ']']))
                    .filter(|t| t.starts_with('-'))
                    .collect();
                (tokens[..n].to_vec(), flags)
            })
            .collect()
    }

    /// Return the words of the help text of `mt` with subcommand `words`, or `None` if `words` is not a command.
    fn help_words(words: &[&str]) -> Option<HashSet<String>> {
        let mut args = words.to_vec();
        args.push("--help");
        let help = Mt::from_args(&["mt"], &args).expect_err("--help exits early");
        help.status.ok()?;
        let split = |c: char| c.is_whitespace() || matches!(c, ',' | '[' | ']');
        Some(help.output.split(split).map(String::from).collect())
    }

    #[test]
    fn rendered_playbooks_have_no_template_markup() {
        for surface in [Surface::Cli, Surface::Mcp] {
            let text = playbook(surface);
            assert!(!text.contains("{{"), "{surface:?}: unresolved term");
            assert!(!text.contains("<!--"), "{surface:?}: block marker");
        }
        let names = TERMS
            .iter()
            .map(|(name, _, _)| *name)
            .chain(TOOL_TERMS.iter().map(|(name, _)| *name))
            .chain(FIELD_TERMS.iter().map(|(name, _)| *name))
            .chain(playbook_steps().into_iter().map(|(name, _)| name));
        for name in names {
            assert!(
                TEMPLATE.contains(&format!("{{{{{name}}}}}")),
                "unused {name}"
            );
        }
    }

    #[test]
    fn cli_playbook_names_only_cli_commands_and_their_flags() {
        let text = playbook(Surface::Cli);
        let commands = cli_commands(&text);
        assert!(commands.len() > 20, "{commands:?}");
        let mut all_flags = HashSet::new();
        for (words, flags) in commands {
            let command = words.join(" ");
            let help =
                help_words(&words).unwrap_or_else(|| panic!("`mt {command}` is not a command"));
            for flag in flags {
                assert!(help.contains(flag), "`mt {command}` has no flag {flag}");
            }
            all_flags.extend(help.into_iter().filter(|w| w.starts_with('-')));
        }
        for span in code_spans(&text) {
            if let Some(flag) = span.split(' ').next().filter(|w| w.starts_with('-')) {
                assert!(all_flags.contains(flag), "no named command has flag {flag}");
            }
        }
        assert!(!text.contains(" { "), "MCP tool syntax in the CLI playbook");
    }

    #[test]
    fn every_cli_step_parses_as_its_command() {
        for step in filled_steps() {
            let args = step.cli_args();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Err(early) = Mt::from_args(&["mt"], &args) {
                panic!("{}: {}", step.render(Surface::Cli), early.output);
            }
        }
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn mcp_playbook_names_only_mcp_tools_and_their_fields() {
        let text = playbook(Surface::Mcp);
        assert!(
            cli_commands(&text).is_empty(),
            "CLI command in the MCP playbook"
        );
        let tools = crate::mcp::tool_fields();
        let mut calls = 0;
        let code_lines = text.match_indices("\n    ").map(|(i, _)| i + 4);
        for i in text.match_indices('`').map(|(i, _)| i).chain(code_lines) {
            let rest = &text[i + 1..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                .collect();
            let after = &rest[name.len()..];
            let is_call = after.starts_with(" {") || text[..i].ends_with("all ");

            if is_call && !name.is_empty() {
                calls += 1;
                let fields = tools
                    .get(&name)
                    .unwrap_or_else(|| panic!("`{name}` is not an MCP tool"));
                let Some(args) = after.strip_prefix(" {") else {
                    continue;
                };
                let args = &args[..args.find('}').expect("closing brace")];
                let mut depth = 0;
                let args = args.split(|c| {
                    match c {
                        '[' => depth += 1,
                        ']' => depth -= 1,
                        _ => {}
                    }
                    c == ',' && depth == 0
                });
                for arg in args.map(str::trim).filter(|a| !a.is_empty()) {
                    let field = arg.split(':').next().unwrap().trim().trim_end_matches('?');
                    assert!(fields.contains(field), "`{name}` has no field {field}");
                }
            }
        }
        assert!(calls > 15, "{calls} tool calls");
    }
}
