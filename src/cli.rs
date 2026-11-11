//! Command-line argument definitions.

use std::path::PathBuf;

use argh::FromArgs;

use crate::surface::{Operation, Tool};
use crate::types;

/// Manage math lessons and spaced-repetition quizzes over a curriculum graph.
#[derive(FromArgs, Debug)]
pub struct Mt {
    /// curriculum directory to use instead of the embedded curriculum (default: `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,

    #[argh(subcommand)]
    pub cmd: Cmd,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum Cmd {
    Path(PathCmd),
    Graph(GraphCmd),
    Lesson(LessonCmd),
    Quiz(QuizCmd),
    Instruct(InstructCmd),
    ImportTurso(ImportTursoCmd),
    #[cfg(feature = "mcp")]
    Mcp(McpCmd),
}

impl Cmd {
    /// Return the agent command of this command, or `None` for an operator command.
    pub fn tool(&self) -> Option<Tool> {
        let tool = match self {
            Cmd::Path(p) => match &p.op {
                PathOp::List(_) => Tool::GetPaths,
                PathOp::New(_) => Tool::NewPath,
                PathOp::State(_) => Tool::GetState,
                PathOp::Next(_) => Tool::GetNext,
                PathOp::Strategy(_) => Tool::SetStrategy,
                PathOp::Subpath(s) => match s.op {
                    SubpathOp::Set(_) => Tool::SubpathSet,
                    SubpathOp::Clear(_) => Tool::SubpathClear,
                },
                PathOp::Syllabus(_) => Tool::GetSyllabus,
                PathOp::Tree(_) => Tool::GetTree,
            },
            Cmd::Graph(g) => match g.op {
                GraphOp::Show(_) => Tool::GetItem,
                GraphOp::List(_) => Tool::GetChildren,
                GraphOp::Check(_) | GraphOp::Dump(_) => return None,
            },
            Cmd::Lesson(l) => match l.op {
                LessonOp::Upsert(_) => Tool::UpsertLesson,
            },
            Cmd::Quiz(q) => match q.op {
                QuizOp::Create(_) => Tool::CreateQuiz,
                QuizOp::Update(_) => Tool::UpdateQuiz,
                QuizOp::Delete(_) => Tool::DeleteQuiz,
                QuizOp::Answer(_) => Tool::AnswerQuiz,
            },
            Cmd::Instruct(_) | Cmd::ImportTurso(_) => return None,
            #[cfg(feature = "mcp")]
            Cmd::Mcp(_) => return None,
        };
        Some(tool)
    }

    /// Return the operation of this command.
    pub fn operation(&self) -> Operation {
        match self {
            Cmd::ImportTurso(_) => Operation::ImportTurso,
            _ => self.tool().map_or(Operation::Repeatable, Tool::operation),
        }
    }
}

/// Manage learning paths.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "path")]
pub struct PathCmd {
    #[argh(subcommand)]
    pub op: PathOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum PathOp {
    List(PathListCmd),
    New(PathNewCmd),
    State(PathStateCmd),
    Next(PathNextCmd),
    Strategy(PathStrategyCmd),
    Subpath(PathSubpathCmd),
    Syllabus(PathSyllabusCmd),
    Tree(PathTreeCmd),
}

/// List all learning paths with goal, creation time, and progress.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "list")]
pub struct PathListCmd {}

/// Create a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "new")]
pub struct PathNewCmd {
    /// learning goal description
    #[argh(positional)]
    pub goal: String,

    /// target atom or cluster IDs, at least 1 (comma-separated or repeated)
    #[argh(option)]
    pub targets: Vec<String>,

    /// initial traversal strategy: `bottom_up` (default) or `top_down`
    #[argh(option, default = "types::Strategy::BottomUp")]
    pub strategy: types::Strategy,
}

/// Show the status summary for a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "state")]
pub struct PathStateCmd {
    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,
}

/// Get the next scheduled action for a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "next")]
pub struct PathNextCmd {
    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// skip due reviews and return the next untaught or incomplete item
    #[argh(switch)]
    pub new: bool,

    /// return only the earliest due review card, or `no_reviews_due` if zero cards are due
    #[argh(switch)]
    pub due: bool,
}

/// List upcoming untaught lesson topics for a path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "syllabus")]
pub struct PathSyllabusCmd {
    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// maximum number of upcoming atoms to return (default: 10)
    #[argh(option, short = 'n', default = "10")]
    pub n: usize,
}

/// Show the prerequisite tree and completion status for a path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "tree")]
pub struct PathTreeCmd {
    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// maximum number of levels below each area (default: unlimited)
    #[argh(option)]
    pub depth: Option<u32>,
}

/// Change the traversal strategy for a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "strategy")]
pub struct PathStrategyCmd {
    /// traversal strategy: `bottom_up` or `top_down`
    #[argh(positional)]
    pub strategy: types::Strategy,

    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,
}

/// Set or clear the prerequisite subpath for a top-down path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "subpath")]
pub struct PathSubpathCmd {
    #[argh(subcommand)]
    pub op: SubpathOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum SubpathOp {
    Set(PathSubpathSetCmd),
    Clear(PathSubpathClearCmd),
}

/// Set the subpath to an ordered sequence of atoms ending at a path target.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "set")]
pub struct PathSubpathSetCmd {
    /// atom IDs in teaching order ending with a target atom (comma-separated or repeated)
    #[argh(option)]
    pub atoms: Vec<String>,

    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,
}

/// Clear the active subpath for a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "clear")]
pub struct PathSubpathClearCmd {
    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,
}

/// Inspect or validate the curriculum graph.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "graph")]
pub struct GraphCmd {
    #[argh(subcommand)]
    pub op: GraphOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum GraphOp {
    Show(GraphShowCmd),
    List(GraphListCmd),
    Check(GraphCheckCmd),
    Dump(GraphDumpCmd),
}

/// Show details for a curriculum atom, cluster, or area.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "show")]
pub struct GraphShowCmd {
    /// atom, cluster, or area ID
    #[argh(positional)]
    pub id: String,

    /// path ID used to include per-atom completion status
    #[argh(option, short = 'p')]
    pub path: Option<String>,
}

/// List areas or child nodes in the curriculum.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "list")]
pub struct GraphListCmd {
    /// parent node ID (omit to list all areas)
    #[argh(positional)]
    pub id: Option<String>,

    /// path ID used to include per-atom completion status
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// also list every descendant of the node
    #[argh(switch)]
    pub recursive: bool,
}

/// Validate the curriculum graph.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "check")]
pub struct GraphCheckCmd {
    /// curriculum directory to validate (default: the curriculum of `--graph`)
    #[argh(option, short = 'p')]
    pub path: Option<PathBuf>,
}

/// Print the user overlay as AYML to stdout.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "dump")]
pub struct GraphDumpCmd {}

/// Manage lessons in the user overlay.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "lesson")]
pub struct LessonCmd {
    #[argh(subcommand)]
    pub op: LessonOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum LessonOp {
    Upsert(LessonUpsertCmd),
}

/// Create or replace the lesson body for an atom.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "upsert")]
pub struct LessonUpsertCmd {
    /// atom ID
    #[argh(positional)]
    pub atom: String,

    /// lesson body text
    #[argh(option)]
    pub body: String,

    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,
}

/// Manage quizzes in the user overlay.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "quiz")]
pub struct QuizCmd {
    #[argh(subcommand)]
    pub op: QuizOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum QuizOp {
    Create(QuizCreateCmd),
    Update(QuizUpdateCmd),
    Delete(QuizDeleteCmd),
    Answer(QuizAnswerCmd),
}

/// Create a quiz for an atom.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "create")]
pub struct QuizCreateCmd {
    /// atom ID
    #[argh(positional)]
    pub atom: String,

    /// difficulty slot: `easy`, `medium`, or `hard`
    #[argh(option)]
    pub difficulty: types::Difficulty,

    /// question text
    #[argh(option)]
    pub question: String,

    /// reference answer text
    #[argh(option)]
    pub answer: String,

    /// optional grading rubric
    #[argh(option)]
    pub rubric: Option<String>,

    /// quiz format: `free_text` (default) or `multiple_choice`
    #[argh(option, long = "type", default = "types::QuizType::FreeText")]
    pub quiz_type: types::QuizType,

    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,
}

/// Update fields on an existing quiz while preserving its ID and FSRS state.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "update")]
pub struct QuizUpdateCmd {
    /// quiz ID
    #[argh(positional)]
    pub quiz: String,

    /// updated question text
    #[argh(option)]
    pub question: Option<String>,

    /// updated reference answer text
    #[argh(option)]
    pub answer: Option<String>,

    /// updated grading rubric
    #[argh(option)]
    pub rubric: Option<String>,

    /// updated difficulty slot: `easy`, `medium`, or `hard`
    #[argh(option)]
    pub difficulty: Option<types::Difficulty>,

    /// updated quiz format: `free_text` or `multiple_choice`
    #[argh(option, long = "type")]
    pub quiz_type: Option<types::QuizType>,

    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,
}

/// Tombstone a quiz so that it is excluded from the merged curriculum.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "delete")]
pub struct QuizDeleteCmd {
    /// quiz ID
    #[argh(positional)]
    pub quiz: String,

    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,
}

/// Record a graded quiz response.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "answer")]
pub struct QuizAnswerCmd {
    /// quiz ID
    #[argh(positional)]
    pub quiz: String,

    /// FSRS grade: `again`, `hard`, `good`, or `easy`
    #[argh(option)]
    pub rating: types::Rating,

    /// verbatim user response
    #[argh(option, long = "user-answer")]
    pub user_answer: Option<String>,

    /// path ID (default: most recent path)
    #[argh(option, short = 'p')]
    pub path: Option<String>,
}

/// Print the embedded agent playbook.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "instruct")]
pub struct InstructCmd {}

/// Import paths, overlay records, and events from a libSQL (Turso) database into Baserunna.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "import-turso")]
pub struct ImportTursoCmd {
    /// libSQL database URL (default: `$TURSO_URL`)
    #[argh(option)]
    pub url: Option<String>,
}

/// Run the MCP HTTP server, or run `mt mcp tools` to print the tool schemas as JSON.
#[cfg(feature = "mcp")]
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "mcp")]
pub struct McpCmd {
    /// bind address (default: `127.0.0.1:8080`)
    #[argh(option, default = "default_mcp_addr()")]
    pub addr: String,

    /// static bearer token for API authentication (overrides `$MT_API_KEY`)
    #[argh(option)]
    pub api_key: Option<String>,

    /// administrator password for OAuth 2.1 login (overrides `$MT_ADMIN_PASSWORD`)
    #[argh(option)]
    pub admin_password: Option<String>,

    /// public base URL advertised in OAuth metadata (overrides `$MT_PUBLIC_URL`)
    #[argh(option)]
    pub public_url: Option<String>,

    /// header that a trusted proxy sets to the client address, such as `Fly-Client-IP` (overrides `$MT_CLIENT_IP_HEADER`; default: the TCP peer address)
    #[argh(option)]
    pub client_ip_header: Option<String>,

    #[argh(subcommand)]
    pub op: Option<McpOp>,
}

#[cfg(feature = "mcp")]
#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum McpOp {
    Tools(McpToolsOp),
}

/// Print the MCP tool schemas as JSON.
#[cfg(feature = "mcp")]
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "tools")]
pub struct McpToolsOp {}

#[cfg(feature = "mcp")]
fn default_mcp_addr() -> String {
    "127.0.0.1:8080".into()
}

/// Split comma-separated and repeated CLI flag values into a flat list of trimmed strings.
pub fn flatten_ids(raw: &[String]) -> Vec<String> {
    raw.iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_agent_command_parses_as_its_tool() {
        let samples: &[&[&str]] = &[
            &["path", "list"],
            &["path", "new", "goal", "--targets", "a"],
            &["path", "strategy", "top_down"],
            &["path", "subpath", "set", "--atoms", "a"],
            &["path", "subpath", "clear"],
            &["path", "next"],
            &["path", "state"],
            &["path", "tree"],
            &["path", "syllabus"],
            &["graph", "show", "a"],
            &["graph", "list"],
            &["lesson", "upsert", "a", "--body", "b"],
            &[
                "quiz",
                "create",
                "a",
                "--difficulty",
                "easy",
                "--question",
                "q",
                "--answer",
                "a",
            ],
            &["quiz", "update", "a.q1"],
            &["quiz", "delete", "a.q1"],
            &["quiz", "answer", "a.q1", "--rating", "good"],
        ];
        let tools: Vec<Tool> = samples
            .iter()
            .map(|args| {
                let cmd = Mt::from_args(&["mt"], args).expect("valid args").cmd;
                let tool = cmd.tool().expect("an agent command");
                assert!(args.join(" ").starts_with(tool.cli()), "{tool:?}: {args:?}");
                assert_eq!(cmd.operation(), tool.operation());
                tool
            })
            .collect();
        assert_eq!(tools, Tool::ALL);
    }

    #[test]
    fn operator_commands_have_no_tool() {
        for args in [&["import-turso"][..], &["graph", "dump"], &["instruct"]] {
            let cmd = Mt::from_args(&["mt"], args).expect("valid args").cmd;
            assert_eq!(cmd.tool(), None);
        }
        let import = Mt::from_args(&["mt"], &["import-turso"]).unwrap().cmd;
        assert_eq!(import.operation(), Operation::ImportTurso);
    }
}
