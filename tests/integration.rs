use std::fs;
use std::sync::Arc;

use assert_cmd::Command;
use mockito::{Matcher, Server};
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use reqwest::blocking::Client;
use strata::ai::{AiConfig, AiRuntime, ContextSelector, HeuristicContextSelector};
use strata::core::{Cell, CellOutput, ExecutionStatus, Language, Notebook, SessionManifest};
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

    let rendered = NotebookStorage::render_smd(&notebook);
    let parsed = NotebookStorage::parse_smd(&rendered).unwrap();

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
    let notebook_path = temp.path().join("demo.smd");
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
        .run_code_cell(&Cell::code(
            Language::Bash,
            "export TARGET=strata\nstrata_export shell_target \"$TARGET\"",
        ))
        .unwrap();
    let bash = session
        .run_code_cell(&Cell::code(Language::Bash, "echo $TARGET"))
        .unwrap();
    let python = session
        .run_code_cell(&Cell::code(
            Language::Python,
            "value = strata.input('shell_target')\nprint(value)",
        ))
        .unwrap();
    let javascript = session
        .run_code_cell(&Cell::code(
            Language::JavaScript,
            "const value = strata.input('shell_target'); console.log(value);",
        ))
        .unwrap();

    assert_eq!(bash.output, "strata");
    assert_eq!(python.output, "strata");
    assert_eq!(javascript.output, "strata");
}

#[test]
fn durable_hydration_restores_language_state() {
    let notebook = Notebook::new("Hydrate").with_cells(vec![
        Cell::code(Language::Python, "value = 99"),
        Cell::code(Language::Python, "print(value)"),
    ]);
    let mut initial = SessionManager::new(&notebook);
    initial
        .register_kernel(Box::new(strata::runtime::PythonKernelAdapter::default()))
        .unwrap();
    initial.run_code_cell(&notebook.cells[0]).unwrap();

    let manifest = initial.manifest.clone();
    let mut resumed = SessionManager::from_manifest(manifest);
    resumed
        .register_kernel(Box::new(strata::runtime::PythonKernelAdapter::default()))
        .unwrap();
    resumed.hydrate().unwrap();

    let record = resumed.run_code_cell(&notebook.cells[1]).unwrap();
    assert_eq!(record.output, "99");
}

#[test]
fn failed_execution_is_summarized() {
    let notebook = Notebook::new("Failures");
    let mut session = SessionManager::new(&notebook);
    session.register_default_kernels().unwrap();

    let record = session
        .run_code_cell(&Cell::code(Language::Python, "raise ValueError('bad')"))
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
    let mut manifest = SessionManifest::new(&notebook);
    manifest
        .named_values
        .insert("shared".to_string(), "hello".to_string());

    let selector = HeuristicContextSelector;
    let bundle = selector.select(&notebook, &manifest, 3, 4);

    assert!(bundle.summary.contains("Selected"));
    assert!(
        bundle
            .snippets
            .iter()
            .any(|snippet| snippet.contains("value = 2"))
    );
    assert!(
        bundle
            .snippets
            .iter()
            .any(|snippet| snippet.contains("shared=hello"))
    );
}

#[test]
fn ai_runtime_uses_models_catalog_and_openai_provider() {
    let mut server = Server::new();
    let _catalog = server
        .mock("GET", "/api.json")
        .with_status(200)
        .with_body(
            r#"{
              "openai": {
                "id": "openai",
                "api": "https://api.openai.com/v1",
                "models": {
                  "gpt-test": {
                    "id": "gpt-test",
                    "modalities": {
                      "input": ["text"],
                      "output": ["text"]
                    }
                  }
                }
              }
            }"#,
        )
        .create();
    let _openai = server
        .mock("POST", "/responses")
        .match_header("authorization", "Bearer test-openai")
        .match_body(Matcher::Regex("gpt-test".to_string()))
        .with_status(200)
        .with_body(r#"{ "output_text": "mocked-openai" }"#)
        .create();

    let notebook = Notebook::new("AI").with_cells(vec![Cell::ai("say hello")]);
    let config = AiConfig {
        preferred_provider: Some("openai".to_string()),
        preferred_model: None,
        models_url: format!("{}/api.json", server.url()),
        openai_api_key: Some("test-openai".to_string()),
        openai_base_url: server.url(),
        anthropic_api_key: None,
        anthropic_base_url: server.url(),
    };
    let mut runtime = AiRuntime::new(
        Client::builder().build().unwrap(),
        config,
        Arc::new(HeuristicContextSelector),
    )
    .unwrap();

    let run = runtime
        .execute(&notebook, &SessionManifest::new(&notebook), 0)
        .unwrap();

    assert_eq!(run.status, ExecutionStatus::Succeeded);
    assert_eq!(run.provider_name, "openai");
    assert_eq!(run.response, "mocked-openai");
}

#[test]
fn ai_runtime_uses_anthropic_provider() {
    let mut server = Server::new();
    let _catalog = server
        .mock("GET", "/api.json")
        .with_status(200)
        .with_body(
            r#"{
              "anthropic": {
                "id": "anthropic",
                "api": "https://api.anthropic.com/v1",
                "models": {
                  "claude-test": {
                    "id": "claude-test",
                    "modalities": {
                      "input": ["text"],
                      "output": ["text"]
                    }
                  }
                }
              }
            }"#,
        )
        .create();
    let _anthropic = server
        .mock("POST", "/messages")
        .match_header("x-api-key", "test-anthropic")
        .match_body(Matcher::Regex("claude-test".to_string()))
        .with_status(200)
        .with_body(r#"{ "content": [ { "type": "text", "text": "mocked-anthropic" } ] }"#)
        .create();

    let notebook = Notebook::new("AI").with_cells(vec![Cell::ai("say hello")]);
    let config = AiConfig {
        preferred_provider: Some("anthropic".to_string()),
        preferred_model: None,
        models_url: format!("{}/api.json", server.url()),
        openai_api_key: None,
        openai_base_url: server.url(),
        anthropic_api_key: Some("test-anthropic".to_string()),
        anthropic_base_url: server.url(),
    };
    let mut runtime = AiRuntime::new(
        Client::builder().build().unwrap(),
        config,
        Arc::new(HeuristicContextSelector),
    )
    .unwrap();

    let run = runtime
        .execute(&notebook, &SessionManifest::new(&notebook), 0)
        .unwrap();

    assert_eq!(run.status, ExecutionStatus::Succeeded);
    assert_eq!(run.provider_name, "anthropic");
    assert_eq!(run.response, "mocked-anthropic");
}

#[test]
fn cli_run_executes_smd_notebook_and_persists_checkpoint() {
    let temp = TempDir::new().unwrap();
    let notebook_path = temp.path().join("flow.smd");
    fs::write(
        &notebook_path,
        r#"<!-- strata:format version=1 -->
<!-- strata:notebook title="Flow" -->

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
    let notebook_path = temp.path().join("js-ts.smd");
    fs::write(
        &notebook_path,
        r#"<!-- strata:format version=1 -->
<!-- strata:notebook title="JS TS" -->

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
        .arg(&notebook_path)
        .assert()
        .success()
        .stdout(contains("7"));
}

#[test]
fn cli_run_updates_smd_outputs_and_execution_counts() {
    let temp = TempDir::new().unwrap();
    let notebook_path = temp.path().join("flow.smd");
    let notebook = Notebook::new("Flow").with_cells(vec![
        Cell::markdown("# Flow"),
        Cell::code(Language::Python, "print('hello from smd')"),
    ]);
    NotebookStorage::save(&notebook_path, &notebook).unwrap();

    Command::cargo_bin("strata")
        .unwrap()
        .arg(&notebook_path)
        .assert()
        .success()
        .stdout(contains("hello from smd"));

    let persisted = NotebookStorage::load(&notebook_path).unwrap();
    assert_eq!(persisted.cells[1].execution_count, Some(1));
    assert!(matches!(
        persisted.cells[1].outputs.first(),
        Some(CellOutput::Stream { .. })
    ));
}

#[test]
fn cli_run_records_ai_failures_without_aborting() {
    let temp = TempDir::new().unwrap();
    let notebook_path = temp.path().join("ai.smd");
    fs::write(
        &notebook_path,
        r#"<!-- strata:format version=1 -->
<!-- strata:notebook title="AI" -->

<!-- strata:cell id=cell-0001 kind=ai language=ai -->
```ai
Explain this notebook
```
"#,
    )
    .unwrap();

    Command::cargo_bin("strata")
        .unwrap()
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .arg(&notebook_path)
        .assert()
        .success()
        .stdout(contains("[ai]").and(contains("Failed")));

    let checkpoint = CheckpointPaths::for_notebook(&notebook_path);
    let manifest = CheckpointStorage::load(&checkpoint).unwrap();
    assert_eq!(manifest.ai_history.len(), 1);
    assert_eq!(manifest.execution_history.len(), 1);
    assert_eq!(
        manifest.execution_history[0].status,
        ExecutionStatus::Failed
    );
}

#[test]
fn cli_imports_ipynb_to_smd() {
    let temp = TempDir::new().unwrap();
    let input = temp.path().join("demo.ipynb");
    let output = temp.path().join("demo.smd");
    let notebook = Notebook::new("Convert").with_cells(vec![
        Cell::markdown("intro"),
        Cell::code(Language::Python, "print('ok')"),
    ]);
    NotebookStorage::save_ipynb(&input, &notebook).unwrap();

    Command::cargo_bin("strata")
        .unwrap()
        .arg("import")
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stdout(contains("Imported"));

    let converted = NotebookStorage::load_smd(&output).unwrap();
    assert_eq!(converted.metadata.title, "Convert");
    assert_eq!(converted.cells.len(), 2);
}

#[test]
fn cli_exports_smd_to_ipynb() {
    let temp = TempDir::new().unwrap();
    let input = temp.path().join("demo.smd");
    let output = temp.path().join("demo.ipynb");
    let notebook = Notebook::new("Convert").with_cells(vec![Cell::code(
        Language::Python,
        "print('ok')",
    )]);
    NotebookStorage::save_smd(&input, &notebook).unwrap();

    Command::cargo_bin("strata")
        .unwrap()
        .arg("export")
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stdout(contains("Exported"));

    let converted = NotebookStorage::load_ipynb(&output).unwrap();
    assert_eq!(converted.metadata.title, "Convert");
    assert_eq!(converted.cells.len(), 1);
}
