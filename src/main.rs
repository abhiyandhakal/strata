use std::path::PathBuf;

use anyhow::Result;

use strata::core::{Cell, Language, Notebook};
use strata::storage::NotebookStorage;
use strata::tui::{App, should_launch_tui};

fn main() -> Result<()> {
    let notebook = match std::env::args().nth(1) {
        Some(path) => NotebookStorage::load_markdown(&PathBuf::from(path))?,
        None => demo_notebook(),
    };

    if should_launch_tui() {
        App::new(notebook).run()?;
    } else {
        println!("{}", NotebookStorage::render_markdown(&notebook));
    }

    Ok(())
}

fn demo_notebook() -> Notebook {
    Notebook::new("Strata Demo").with_cells(vec![
        Cell::text("Strata is a programmable terminal notebook."),
        Cell::code(Language::Bash, "export NAME=strata\necho $NAME"),
        Cell::code(Language::Python, "value = 42\nprint(value)"),
        Cell::ai("rewrite the Python snippet in Rust"),
    ])
}
