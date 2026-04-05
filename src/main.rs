use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use strata::config::StrataConfig;
use strata::runtime::{
    load_session_for_notebook, run_notebook_cells, summarize_records,
};
use strata::storage::{CheckpointPaths, CheckpointStorage, NotebookFormat, NotebookStorage};
use strata::theme::ThemeResolver;
use strata::tui::{App, should_launch_tui};

#[derive(Parser)]
#[command(name = "strata")]
#[command(
    about = "Terminal-native notebook runner and editor",
    long_about = "Strata works directly with `.smd` notebooks.\n\nUse `strata <notebook.smd>` to open the notebook UI in a real terminal, or to execute the notebook headlessly in a non-interactive environment.\n\nUse `import` and `export` to convert between `.ipynb` and `.smd`.",
    override_usage = "strata <notebook.smd>\n       strata import <input.ipynb> [output.smd]\n       strata export <input.smd> [output.ipynb]",
    after_help = "Examples:\n  strata notes.smd\n  strata import analysis.ipynb\n  strata import analysis.ipynb analysis.smd\n  strata export analysis.smd\n  strata export analysis.smd analysis.ipynb"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[arg(help = "Path to a Strata `.smd` notebook to open or run", value_name = "NOTEBOOK")]
    path: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Convert a Jupyter `.ipynb` notebook into a Strata `.smd` notebook")]
    Import {
        #[arg(help = "Input Jupyter notebook", value_name = "INPUT_IPYNB")]
        input: PathBuf,
        #[arg(help = "Optional output `.smd` path", value_name = "OUTPUT_SMD")]
        output: Option<PathBuf>,
    },
    #[command(about = "Convert a Strata `.smd` notebook into a Jupyter `.ipynb` notebook")]
    Export {
        #[arg(help = "Input Strata notebook", value_name = "INPUT_SMD")]
        input: PathBuf,
        #[arg(help = "Optional output `.ipynb` path", value_name = "OUTPUT_IPYNB")]
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
    let (mut session, reconcile_notice) = load_session_for_notebook(&path, &mut notebook)?;

    if should_launch_tui() {
        let resolution =
            ThemeResolver::from_env().resolve(config.theme.path.as_deref(), Some(&path));
        let startup_notice = match (resolution.warning, reconcile_notice) {
            (Some(theme), Some(reconcile)) => Some(format!("{theme}; {reconcile}")),
            (Some(theme), None) => Some(theme),
            (None, Some(reconcile)) => Some(reconcile),
            (None, None) => None,
        };
        App::new(
            notebook,
            Some(path),
            session,
            config.editor.vim_mode,
            resolution.theme,
            startup_notice,
        )
        .run()?;
    } else {
        let checkpoint_paths = CheckpointPaths::for_notebook(&path);
        if let Some(notice) = reconcile_notice {
            println!("{notice}");
        }
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
