use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use strata::config::StrataConfig;
use strata::runtime::{
    load_session_for_notebook, run_notebook_cells, summarize_records,
};
use strata::storage::{CheckpointPaths, CheckpointStorage, NotebookStorage};
use strata::tui::{App, should_launch_tui};

#[derive(Parser)]
#[command(name = "strata")]
#[command(about = "Terminal-native structured execution")]
struct Cli {
    path: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    open_or_run_command(cli.path)
}

fn open_or_run_command(path: PathBuf) -> Result<()> {
    let config = StrataConfig::load()?;
    let mut notebook = NotebookStorage::load(&path)?;
    let mut session = load_session_for_notebook(&path, &notebook)?;

    if should_launch_tui() {
        App::new(notebook, Some(path), session, config.editor.vim_mode).run()?;
    } else {
        let checkpoint_paths = CheckpointPaths::for_notebook(&path);
        let records = run_notebook_cells(&mut session, &mut notebook)
            .with_context(|| format!("failed to execute notebook {}", path.display()))?;
        CheckpointStorage::save(&checkpoint_paths, &session.manifest)?;
        NotebookStorage::save(&path, &notebook)?;
        session.shutdown()?;

        if records.is_empty() {
            println!("No executable cells found.");
        } else {
            println!("{}", summarize_records(&records));
        }
    }

    Ok(())
}
