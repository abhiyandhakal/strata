use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use strata::ai::AiRuntime;
use strata::core::{Cell, Language, Notebook};
use strata::runtime::{
    SessionManager, load_session_for_notebook, run_notebook_cells, summarize_records,
};
use strata::storage::{CheckpointPaths, CheckpointStorage, NotebookStorage};
use strata::tui::{App, should_launch_tui};

#[derive(Parser)]
#[command(name = "strata")]
#[command(about = "Terminal-native structured execution")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Open { path: Option<PathBuf> },
    Run { path: PathBuf },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Open { path: None }) {
        Commands::Open { path } => open_command(path),
        Commands::Run { path } => run_command(path),
    }
}

fn open_command(path: Option<PathBuf>) -> Result<()> {
    let (notebook, notebook_path, session) = match path {
        Some(path) => {
            let notebook = NotebookStorage::load_markdown(&path)?;
            let session = load_session_for_notebook(&path, &notebook)?;
            (notebook, Some(path), session)
        }
        None => {
            let notebook = demo_notebook();
            let session = SessionManager::new(&notebook).with_ai_runtime(AiRuntime::from_env()?);
            (notebook, None, session)
        }
    };

    if should_launch_tui() {
        App::new(notebook, notebook_path, session).run()?;
    } else {
        println!("{}", NotebookStorage::render_markdown(&notebook));
    }

    Ok(())
}

fn run_command(path: PathBuf) -> Result<()> {
    let notebook = NotebookStorage::load_markdown(&path)?;
    let checkpoint_paths = CheckpointPaths::for_notebook(&path);
    let mut session = load_session_for_notebook(&path, &notebook)?;

    let records = run_notebook_cells(&mut session, &notebook)
        .with_context(|| format!("failed to execute notebook {}", path.display()))?;
    CheckpointStorage::save(&checkpoint_paths, &session.manifest)?;
    session.shutdown()?;

    if records.is_empty() {
        println!("No executable cells found.");
    } else {
        println!("{}", summarize_records(&records));
    }

    Ok(())
}

fn demo_notebook() -> Notebook {
    Notebook::new("Strata Demo").with_cells(vec![
        Cell::text("Strata is a programmable terminal notebook."),
        Cell::code(
            Language::Bash,
            "export NAME=strata\nstrata_export shell_name \"$NAME\"\necho $NAME",
        ),
        Cell::code(
            Language::Python,
            "value = 42\nprint(value)\nstrata.export('python_value', value)",
        ),
        Cell::code(
            Language::JavaScript,
            "const label = strata.input('shell_name'); console.log(label);",
        ),
        Cell::ai("rewrite the Python snippet in Rust"),
    ])
}
