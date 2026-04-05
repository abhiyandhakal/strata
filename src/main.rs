use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use strata::config::StrataConfig;
use strata::runtime::{
    load_session_for_notebook, run_notebook_cells, summarize_records,
};
use strata::storage::{CheckpointPaths, CheckpointStorage, NotebookFormat, NotebookStorage};
use strata::tui::{App, should_launch_tui};

#[derive(Parser)]
#[command(name = "strata")]
#[command(about = "Terminal-native structured execution")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    path: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    Import {
        input: PathBuf,
        output: Option<PathBuf>,
    },
    Export {
        input: PathBuf,
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match (cli.command, cli.path) {
        (Some(Commands::Import { input, output }), None) => import_command(input, output),
        (Some(Commands::Export { input, output }), None) => export_command(input, output),
        (None, Some(path)) => open_or_run_command(path),
        _ => anyhow::bail!("usage: strata <path.smd> | strata import <file.ipynb> [output.smd] | strata export <file.smd> [output.ipynb]"),
    }
}

fn open_or_run_command(path: PathBuf) -> Result<()> {
    if NotebookStorage::format_for_path(&path) != NotebookFormat::Smd {
        anyhow::bail!(
            "strata opens `.smd` notebooks directly; import/export `.ipynb` explicitly first"
        );
    }
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

fn import_command(input: PathBuf, output: Option<PathBuf>) -> Result<()> {
    if NotebookStorage::format_for_path(&input) != NotebookFormat::Ipynb {
        anyhow::bail!("import expects an `.ipynb` input file");
    }
    let notebook = NotebookStorage::load_ipynb(&input)?;
    let output = output.unwrap_or_else(|| input.with_extension(NotebookFormat::Smd.extension()));
    NotebookStorage::save_smd(&output, &notebook)?;
    println!("Imported {} -> {}", input.display(), output.display());
    Ok(())
}

fn export_command(input: PathBuf, output: Option<PathBuf>) -> Result<()> {
    if NotebookStorage::format_for_path(&input) != NotebookFormat::Smd {
        anyhow::bail!("export expects an `.smd` input file");
    }
    let notebook = NotebookStorage::load_smd(&input)?;
    let output =
        output.unwrap_or_else(|| input.with_extension(NotebookFormat::Ipynb.extension()));
    NotebookStorage::save_ipynb(&output, &notebook)?;
    println!("Exported {} -> {}", input.display(), output.display());
    Ok(())
}
