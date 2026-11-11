//! Agent playbook for each surface, rendered from one template.

use crate::Result;
use crate::surface::{DIFFICULTIES, Field, QUIZ_TYPES, RATINGS, Step, Surface, Tool, Value};

/// Playbook template.
///
/// A line `<!-- cli -->` or `<!-- mcp -->` starts a block for that surface only, and `<!-- end -->` ends it. Each `{{name}}` is a term of [`TERMS`] or a step of [`playbook_steps`].
const TEMPLATE: &str = include_str!("playbook.md");

/// Name, CLI form, and MCP form of each term in [`TEMPLATE`].
const TERMS: &[(&str, &str, &str)] = &[
    ("surface", "the `mt` CLI", "the Math Tutor MCP server"),
    ("server", "`mt`", "The MCP server"),
    ("step", "command", "tool call"),
    ("bottom_up", "bottom-up", "bottom_up"),
    ("top_down", "top-down", "top_down"),
    (
        "set_strategy",
        "Pass `--strategy top-down` to `mt path new`, or change the strategy at any time with `mt path strategy <bottom-up|top-down>`",
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
    ("next_name", "`mt path next`", "`get_next`"),
    (
        "inspect_target",
        "Run `mt graph show <target>`",
        "Call `get_item { id: <target> }`",
    ),
    (
        "subpath_set",
        "Run `mt path subpath set --atoms <prereq>,<...>,<target>`",
        "Call `subpath_set { path_id, atoms }` with an ordered list of prerequisite atoms",
    ),
    (
        "subpath_set_again",
        "run `mt path subpath set` again",
        "call `subpath_set` again",
    ),
    (
        "subpath_clear",
        "run `mt path subpath clear`",
        "call `subpath_clear { path_id }`",
    ),
    ("quiz_type", "`--type`", "`kind`"),
    ("answer_name", "`mt quiz answer`", "`answer_quiz`"),
    ("verbatim_field", "`--user-answer`", "`answer`"),
    ("run", "Run", "Call"),
    (
        "delete_quiz",
        "Run `mt quiz delete <quiz-id>`",
        "Call `delete_quiz { quiz_id, path_id }`",
    ),
    ("update_quiz_name", "`mt quiz update`", "`update_quiz`"),
    ("delete_quiz_name", "`mt quiz delete`", "`delete_quiz`"),
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

/// Return the steps with free text that the playbook names, each with its term name.
pub fn playbook_steps() -> Vec<(&'static str, Step)> {
    let path = || Value::Id("path-id");
    let atom = || Value::Id("atom-id");
    let quiz = || Value::Id("quiz-id");
    vec![
        (
            "upsert_lesson",
            Step::new(Tool::UpsertLesson, path())
                .arg(Field::Atom, atom())
                .arg(Field::Body, Value::Text("lesson")),
        ),
        (
            "create_quiz",
            Step::new(Tool::CreateQuiz, path())
                .arg(Field::Atom, atom())
                .arg(Field::Difficulty, Value::Choice(DIFFICULTIES))
                .arg(Field::Question, Value::Text("question"))
                .arg(Field::Answer, Value::Text("answer"))
                .optional(Field::Rubric, Value::Text("rubric"))
                .optional(Field::QuizType, Value::Choice(QUIZ_TYPES)),
        ),
        (
            "answer_quiz",
            Step::new(Tool::AnswerQuiz, path())
                .arg(Field::QuizId, quiz())
                .arg(Field::Rating, Value::Choice(RATINGS))
                .arg(Field::UserAnswer, Value::Text("verbatim reply")),
        ),
        (
            "update_quiz",
            Step::new(Tool::UpdateQuiz, path())
                .arg(Field::QuizId, quiz())
                .optional(Field::Question, Value::Text("question"))
                .optional(Field::Answer, Value::Text("answer"))
                .optional(Field::Rubric, Value::Text("rubric"))
                .optional(Field::Difficulty, Value::Choice(DIFFICULTIES))
                .optional(Field::QuizType, Value::Choice(QUIZ_TYPES)),
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
    for (name, step) in playbook_steps() {
        out = out.replace(&format!("{{{{{name}}}}}"), &step.render(surface));
    }
    out
}

/// Print the CLI agent playbook (`mt instruct`).
pub fn cmd_instruct() -> Result<()> {
    print!("{}", playbook(Surface::Cli));
    Ok(())
}

#[cfg(test)]
mod tests {
    use argh::FromArgs;

    use super::*;
    use crate::cli::Mt;

    /// Return the subcommand words of each `mt` command that `text` names in code.
    fn cli_commands(text: &str) -> Vec<Vec<&str>> {
        let mut out = Vec::new();
        for line in text.lines() {
            let mut starts: Vec<usize> = line.match_indices("`mt ").map(|(i, _)| i + 4).collect();
            if let Some(code) = line.strip_prefix("    mt ") {
                starts.push(line.len() - code.len());
            }
            for start in starts {
                let words: Vec<&str> = line[start..]
                    .split(' ')
                    .take_while(|w| {
                        w.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                            && w.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                    })
                    .collect();
                out.push(words);
            }
        }
        out
    }

    #[test]
    fn rendered_playbooks_have_no_template_markup() {
        for surface in [Surface::Cli, Surface::Mcp] {
            let text = playbook(surface);
            assert!(!text.contains("{{"), "{surface:?}: unresolved term");
            assert!(!text.contains("<!--"), "{surface:?}: block marker");
        }
        let steps = playbook_steps().into_iter().map(|(name, _)| name);
        for name in TERMS.iter().map(|(name, _, _)| *name).chain(steps) {
            assert!(
                TEMPLATE.contains(&format!("{{{{{name}}}}}")),
                "unused {name}"
            );
        }
    }

    #[test]
    fn cli_playbook_names_only_cli_commands() {
        let text = playbook(Surface::Cli);
        let commands = cli_commands(&text);
        assert!(commands.len() > 20, "{commands:?}");
        for words in commands {
            let mut args = words.clone();
            args.push("--help");
            let help = Mt::from_args(&["mt"], &args).expect_err("--help exits early");
            assert!(
                help.status.is_ok(),
                "`mt {}` is not a command",
                words.join(" ")
            );
        }
        assert!(!text.contains(" { "), "MCP tool syntax in the CLI playbook");
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn mcp_playbook_names_only_mcp_tools() {
        let text = playbook(Surface::Mcp);
        assert!(
            cli_commands(&text).is_empty(),
            "CLI command in the MCP playbook"
        );
        let tools = crate::mcp::tool_names();
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
                assert!(tools.contains(&name), "`{name}` is not an MCP tool");
            }
        }
        assert!(calls > 15, "{calls} tool calls");
    }
}
