use strata::ai::{ContextSelector, HeuristicContextSelector};
use strata::core::{Cell, Language, Notebook, SessionManifest};
use strata::runtime::{BashKernelAdapter, PythonKernelAdapter, SessionManager};
use strata::storage::{CheckpointPaths, CheckpointStorage, NotebookStorage};

#[test]
fn notebook_markdown_round_trip_keeps_shapes() {
    let notebook = Notebook::new("Integration").with_cells(vec![
        Cell::text("overview"),
        Cell::code(Language::Bash, "echo hi"),
        Cell::ai("summarize the shell output"),
    ]);

    let rendered = NotebookStorage::render_markdown(&notebook);
    let parsed = NotebookStorage::parse_markdown(&rendered).unwrap();

    assert_eq!(parsed.cells.len(), 3);
    assert_eq!(parsed.cells[1].language, Language::Bash);
    assert_eq!(parsed.cells[2].language, Language::Ai);
}

#[test]
fn checkpoint_storage_round_trip_works() {
    let notebook = Notebook::new("Checkpoint");
    let manifest = SessionManifest::new(&notebook);
    let root = std::env::temp_dir().join(format!("strata-test-{}", std::process::id()));
    let notebook_path = root.join("demo.strata.md");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&notebook_path, "# Demo").unwrap();

    let paths = CheckpointPaths::for_notebook(&notebook_path);
    CheckpointStorage::save(&paths, &manifest).unwrap();
    let loaded = CheckpointStorage::load(&paths).unwrap();

    assert_eq!(loaded.session_id, manifest.session_id);
    std::fs::remove_dir_all(root.join(".strata")).ok();
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn session_manager_runs_stateful_kernels() {
    let notebook = Notebook::new("Runtime");
    let mut session = SessionManager::new(&notebook);
    session
        .register_kernel(Box::new(BashKernelAdapter::default()))
        .unwrap();
    session
        .register_kernel(Box::new(PythonKernelAdapter::default()))
        .unwrap();

    session
        .run_cell(&Cell::code(Language::Bash, "export TARGET=strata"))
        .unwrap();
    let bash = session
        .run_cell(&Cell::code(Language::Bash, "echo $TARGET"))
        .unwrap();
    let python = session
        .run_cell(&Cell::code(Language::Python, "value = 7\nprint(value)"))
        .unwrap();

    assert_eq!(bash.output, "strata");
    assert_eq!(python.output, "7");
}

#[test]
fn ai_selector_builds_context_bundle() {
    let notebook = Notebook::new("AI").with_cells(vec![
        Cell::text("intro"),
        Cell::code(Language::Bash, "echo one"),
        Cell::code(Language::Python, "value = 2"),
        Cell::ai("explain value"),
    ]);

    let selector = HeuristicContextSelector;
    let bundle = selector.select(&notebook.cells, 3, 4);

    assert!(bundle.summary.contains("Selected"));
    assert!(
        bundle
            .snippets
            .iter()
            .any(|snippet| snippet.contains("value = 2"))
    );
}
