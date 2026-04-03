use std::fs;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use strata::ai::{ContextSelector, HeuristicContextSelector};
use strata::core::{Cell, ExecutionStatus, Language, Notebook, SessionManifest};
use strata::runtime::{SessionManager, summarize_records};
use strata::storage::{CheckpointPaths, CheckpointStorage, NotebookStorage};
use tempfile::TempDir;

#[test]
fn notebook_markdown_round_trip_keeps_shapes() {
    let notebook = Notebook::new("Integration").with_cells(vec![
        Cell::text("overview"),
        Cell::code(Language::Bash, "echo hi"),
        Cell::code(Language::TypeScript, "const value: number = 1;"),
        Cell::ai("summarize the shell output"),
    ]);

    let rendered = NotebookStorage::render_markdown(&notebook);
    let parsed = NotebookStorage::parse_markdown(&rendered).unwrap();

    assert_eq!(parsed.cells.len(), 4);
    assert_eq!(parsed.cells[1].language, Language::Bash);
    assert_eq!(parsed.cells[2].language, Language::TypeScript);
    assert_eq!(parsed.cells[3].language, Language::Ai);
}

#[test]
fn checkpoint_storage_round_trip_works() {
    let notebook = Notebook::new("Checkpoint");
    let mut manifest = SessionManifest::new(&notebook);
    manifest
        .named_values
        .insert("shared".to_string(), "value".to_string());
    let temp = TempDir::new().unwrap();
    let notebook_path = temp.path().join("demo.strata.md");
    fs::write(&notebook_path, "# Demo").unwrap();

    let paths = CheckpointPaths::for_notebook(&notebook_path);
    CheckpointStorage::save(&paths, &manifest).unwrap();
    let loaded = CheckpointStorage::load(&paths).unwrap();

    assert_eq!(loaded.session_id, manifest.session_id);
    assert_eq!(
        loaded.named_values.get("shared"),
        Some(&"value".to_string())
    );
}

#[test]
fn session_manager_runs_stateful_kernels() {
    let notebook = Notebook::new("Runtime");
    let mut session = SessionManager::new(&notebook);
    session.register_default_kernels().unwrap();

    session
        .run_cell(&Cell::code(
            Language::Bash,
            "export TARGET=strata\nstrata_export shell_target \"$TARGET\"",
        ))
        .unwrap();
    let bash = session
        .run_cell(&Cell::code(Language::Bash, "echo $TARGET"))
        .unwrap();
    let python = session
        .run_cell(&Cell::code(
            Language::Python,
            "value = strata.input('shell_target')\nprint(value)",
        ))
        .unwrap();
    let javascript = session
        .run_cell(&Cell::code(
            Language::JavaScript,
            "const value = strata.input('shell_target'); console.log(value);",
        ))
        .unwrap();

    assert_eq!(bash.output, "strata");
    assert_eq!(python.output, "strata");
    assert_eq!(javascript.output, "strata");
}

#[test]
fn failed_execution_is_summarized() {
    let notebook = Notebook::new("Failures");
    let mut session = SessionManager::new(&notebook);
    session.register_default_kernels().unwrap();

    let record = session
        .run_cell(&Cell::code(Language::Python, "raise ValueError('bad')"))
        .unwrap();
    let summary = summarize_records(std::slice::from_ref(&record));

    assert_eq!(record.status, ExecutionStatus::Failed);
    assert!(summary.contains("exit"));
    assert!(summary.contains("ValueError"));
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

#[test]
fn cli_run_executes_notebook_and_persists_checkpoint() {
    let temp = TempDir::new().unwrap();
    let notebook_path = temp.path().join("flow.md");
    fs::write(
        &notebook_path,
        r#"# Flow

<!-- strata:cell id=cell-0001 kind=code language=python -->
```python
strata.export("shared", "hello")
print("python-ready")
```

<!-- strata:cell id=cell-0002 kind=code language=bash -->
```bash
echo $(strata_input shared)
```

<!-- strata:cell id=cell-0003 kind=code language=javascript -->
```javascript
console.log(strata.input("shared"))
```
"#,
    )
    .unwrap();

    Command::cargo_bin("strata")
        .unwrap()
        .arg("run")
        .arg(&notebook_path)
        .assert()
        .success()
        .stdout(contains("python-ready").and(contains("hello")));

    let checkpoint = CheckpointPaths::for_notebook(&notebook_path);
    let manifest = CheckpointStorage::load(&checkpoint).unwrap();
    assert_eq!(
        manifest.named_values.get("shared"),
        Some(&"hello".to_string())
    );
    assert_eq!(manifest.execution_history.len(), 3);
}

#[test]
fn cli_run_handles_javascript_and_typescript_cells() {
    let temp = TempDir::new().unwrap();
    let notebook_path = temp.path().join("js-ts.md");
    fs::write(
        &notebook_path,
        r#"# JS TS

<!-- strata:cell id=cell-0001 kind=code language=javascript -->
```javascript
globalThis.count = 2;
```

<!-- strata:cell id=cell-0002 kind=code language=typescript -->
```typescript
globalThis.count += 5;
console.log(globalThis.count);
```
"#,
    )
    .unwrap();

    Command::cargo_bin("strata")
        .unwrap()
        .arg("run")
        .arg(&notebook_path)
        .assert()
        .success()
        .stdout(contains("7"));
}
