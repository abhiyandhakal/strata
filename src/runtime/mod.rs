use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::ai::AiRuntime;
use crate::core::{
    ArtifactId, ArtifactRef, BridgeValue, Cell, CellKind, ExecutionId, ExecutionRecord,
    ExecutionRequest, ExecutionStatus, Language, Notebook, SessionId, SessionManifest,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelExecution {
    pub output: String,
    pub error_output: String,
    pub exit_code: i32,
    pub bridges: Vec<BridgeValue>,
    pub artifacts: Vec<ArtifactRef>,
}

pub trait KernelAdapter: Send {
    fn supported_languages(&self) -> &[Language];
    fn start(&mut self) -> Result<()>;
    fn execute(&mut self, request: &ExecutionRequest) -> Result<KernelExecution>;
    fn interrupt(&mut self) -> Result<()>;
    fn restart(&mut self) -> Result<()>;
    fn shutdown(&mut self) -> Result<()>;
}

pub struct SessionManager {
    pub session_id: SessionId,
    kernels: Vec<Box<dyn KernelAdapter>>,
    language_map: HashMap<Language, usize>,
    pub manifest: SessionManifest,
    ai_runtime: Option<AiRuntime>,
}

impl SessionManager {
    pub fn new(notebook: &Notebook) -> Self {
        Self::from_manifest(SessionManifest::new(notebook))
    }

    pub fn from_manifest(manifest: SessionManifest) -> Self {
        Self {
            session_id: manifest.session_id.clone(),
            kernels: Vec::new(),
            language_map: HashMap::new(),
            manifest,
            ai_runtime: None,
        }
    }

    pub fn with_ai_runtime(mut self, ai_runtime: AiRuntime) -> Self {
        self.ai_runtime = Some(ai_runtime);
        self
    }

    pub fn register_kernel(&mut self, mut kernel: Box<dyn KernelAdapter>) -> Result<()> {
        kernel.start()?;
        let index = self.kernels.len();
        for language in kernel.supported_languages() {
            self.language_map.insert(*language, index);
        }
        self.kernels.push(kernel);
        Ok(())
    }

    pub fn register_default_kernels(&mut self) -> Result<()> {
        self.register_kernel(Box::new(BashKernelAdapter::default()))?;
        self.register_kernel(Box::new(PythonKernelAdapter::default()))?;
        self.register_kernel(Box::new(JavaScriptKernelAdapter::default()))?;
        Ok(())
    }

    pub fn ensure_ai_runtime(&mut self) -> Result<()> {
        if self.ai_runtime.is_none() {
            self.ai_runtime = Some(AiRuntime::from_env()?);
        }
        Ok(())
    }

    pub fn hydrate(&mut self) -> Result<()> {
        let mut replay_named_values = BTreeMap::new();

        for record in self
            .manifest
            .execution_history
            .iter()
            .filter(|record| record.status == ExecutionStatus::Succeeded)
        {
            match record.language {
                Language::Bash | Language::Python | Language::JavaScript | Language::TypeScript => {
                    let Some(index) = self.language_map.get(&record.language).copied() else {
                        continue;
                    };
                    let request = ExecutionRequest {
                        cell_id: record.cell_id.clone(),
                        language: record.language,
                        source: record.source.clone(),
                        named_values: replay_named_values.clone(),
                    };
                    let execution = self.kernels[index]
                        .execute(&request)
                        .with_context(|| format!("failed to hydrate {}", record.cell_id))?;
                    apply_bridges(&mut replay_named_values, &execution.bridges);
                }
                Language::Ai | Language::Text => {}
            }
        }

        self.manifest.named_values = replay_named_values;
        Ok(())
    }

    pub fn run_cell_at(&mut self, notebook: &Notebook, index: usize) -> Result<ExecutionRecord> {
        let cell = notebook
            .cells
            .get(index)
            .context("cell index out of bounds")?;
        match cell.kind {
            CellKind::Code => self.run_code_cell(cell),
            CellKind::Ai => self.run_ai_cell(notebook, index),
            CellKind::Text => bail!("text cells are not executable"),
        }
    }

    pub fn run_code_cell(&mut self, cell: &Cell) -> Result<ExecutionRecord> {
        let request = ExecutionRequest {
            cell_id: cell.id.clone(),
            language: cell.language,
            source: cell.source.clone(),
            named_values: self.manifest.named_values.clone(),
        };

        let Some(index) = self.language_map.get(&cell.language).copied() else {
            bail!("no kernel registered for {:?}", cell.language);
        };
        let execution = self.kernels[index].execute(&request)?;
        apply_bridges(&mut self.manifest.named_values, &execution.bridges);
        self.manifest.artifacts.extend(execution.artifacts.clone());

        let record = ExecutionRecord {
            id: ExecutionId::new(),
            cell_id: cell.id.clone(),
            language: cell.language,
            source: cell.source.clone(),
            status: if execution.exit_code == 0 {
                ExecutionStatus::Succeeded
            } else {
                ExecutionStatus::Failed
            },
            output: execution.output,
            error_output: execution.error_output,
            exit_code: execution.exit_code,
            dependencies: execution.artifacts.clone(),
            bridges: execution.bridges,
        };
        self.manifest.execution_history.push(record.clone());
        Ok(record)
    }

    pub fn run_ai_cell(&mut self, notebook: &Notebook, index: usize) -> Result<ExecutionRecord> {
        let ai_run = match self
            .ensure_ai_runtime()
            .and_then(|_| self.ai_runtime.as_mut().context("AI runtime unavailable"))
            .and_then(|ai_runtime| ai_runtime.execute(notebook, &self.manifest, index))
        {
            Ok(run) => run,
            Err(error) => crate::core::AiRunRecord {
                prompt_cell_id: notebook.cells[index].id.0.clone(),
                prompt: notebook.cells[index].source.clone(),
                context: crate::core::ContextBundle {
                    summary: "AI execution failed before context resolution completed".to_string(),
                    cell_ids: vec![notebook.cells[index].id.0.clone()],
                    snippets: vec![notebook.cells[index].source.clone()],
                },
                provider_name: self
                    .ai_runtime
                    .as_ref()
                    .and_then(|_| std::env::var("STRATA_AI_PROVIDER").ok())
                    .unwrap_or_else(|| "unconfigured".to_string()),
                model_id: std::env::var("STRATA_AI_MODEL")
                    .unwrap_or_else(|_| "unspecified".to_string()),
                response: String::new(),
                error_output: error.to_string(),
                status: ExecutionStatus::Failed,
            },
        };
        let record = ExecutionRecord {
            id: ExecutionId::new(),
            cell_id: notebook.cells[index].id.clone(),
            language: Language::Ai,
            source: notebook.cells[index].source.clone(),
            status: ai_run.status.clone(),
            output: ai_run.response.clone(),
            error_output: ai_run.error_output.clone(),
            exit_code: if ai_run.status == ExecutionStatus::Succeeded {
                0
            } else {
                1
            },
            dependencies: Vec::new(),
            bridges: Vec::new(),
        };
        self.manifest.ai_history.push(ai_run);
        self.manifest.execution_history.push(record.clone());
        Ok(record)
    }

    pub fn latest_record_for_cell(&self, cell_id: &str) -> Option<&ExecutionRecord> {
        self.manifest
            .execution_history
            .iter()
            .rev()
            .find(|record| record.cell_id.0 == cell_id)
    }

    pub fn shutdown(&mut self) -> Result<()> {
        for kernel in &mut self.kernels {
            kernel.shutdown()?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct WorkerRequest {
    cell_id: String,
    language: String,
    source: String,
    inputs: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct WorkerResponse {
    output: String,
    error_output: String,
    exit_code: i32,
    bridges: Vec<WorkerBridge>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WorkerBridge {
    Environment { key: String, value: String },
    NamedValue { name: String, value: String },
    Artifact { name: String, path: String },
}

#[derive(Debug)]
struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

struct WorkerKernelAdapter {
    languages: Vec<Language>,
    program: String,
    args: Vec<String>,
    child: Option<WorkerProcess>,
}

impl WorkerKernelAdapter {
    fn new(languages: Vec<Language>, program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            languages,
            program: program.into(),
            args,
            child: None,
        }
    }

    fn start_process(&mut self) -> Result<()> {
        if self.child.is_some() {
            return Ok(());
        }

        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start kernel process {}", self.program))?;
        let stdin = child
            .stdin
            .take()
            .context("kernel child stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("kernel child stdout unavailable")?;
        self.child = Some(WorkerProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        });
        Ok(())
    }

    fn stop_process(&mut self) -> Result<()> {
        let Some(mut process) = self.child.take() else {
            return Ok(());
        };
        let shutdown = serde_json::json!({ "command": "shutdown" }).to_string();
        let _ = writeln!(process.stdin, "{shutdown}");
        let _ = process.stdin.flush();
        let _ = process.child.wait();
        Ok(())
    }
}

impl KernelAdapter for WorkerKernelAdapter {
    fn supported_languages(&self) -> &[Language] {
        &self.languages
    }

    fn start(&mut self) -> Result<()> {
        self.start_process()
    }

    fn execute(&mut self, request: &ExecutionRequest) -> Result<KernelExecution> {
        self.start_process()?;
        let process = self.child.as_mut().context("kernel child missing")?;
        let payload = WorkerRequest {
            cell_id: request.cell_id.0.clone(),
            language: request.language.fence_name().to_string(),
            source: request.source.clone(),
            inputs: request.named_values.clone(),
        };
        let encoded = serde_json::to_string(&payload)?;
        writeln!(process.stdin, "{encoded}")?;
        process.stdin.flush()?;

        let mut response_line = String::new();
        let read = process.stdout.read_line(&mut response_line)?;
        if read == 0 {
            bail!("kernel process exited unexpectedly");
        }
        let response: WorkerResponse = serde_json::from_str(response_line.trim_end())
            .context("failed to decode kernel response")?;

        let mut bridges = Vec::new();
        let mut artifacts = Vec::new();
        for bridge in response.bridges {
            match bridge {
                WorkerBridge::Environment { key, value } => {
                    bridges.push(BridgeValue::Environment { key, value });
                }
                WorkerBridge::NamedValue { name, value } => {
                    bridges.push(BridgeValue::NamedValue { name, value });
                }
                WorkerBridge::Artifact { name, path } => {
                    let artifact = ArtifactRef {
                        id: ArtifactId::new(),
                        name,
                        path,
                    };
                    artifacts.push(artifact.clone());
                    bridges.push(BridgeValue::Artifact(artifact));
                }
            }
        }

        if !response.output.is_empty() {
            bridges.push(BridgeValue::Stdout(response.output.clone()));
        }

        Ok(KernelExecution {
            output: response.output,
            error_output: response.error_output,
            exit_code: response.exit_code,
            bridges,
            artifacts,
        })
    }

    fn interrupt(&mut self) -> Result<()> {
        self.restart()
    }

    fn restart(&mut self) -> Result<()> {
        self.stop_process()?;
        self.start_process()
    }

    fn shutdown(&mut self) -> Result<()> {
        self.stop_process()
    }
}

pub struct BashKernelAdapter {
    inner: WorkerKernelAdapter,
}

impl Default for BashKernelAdapter {
    fn default() -> Self {
        Self {
            inner: WorkerKernelAdapter::new(
                vec![Language::Bash],
                "python3",
                vec![kernel_script("scripts/bash_kernel.py")],
            ),
        }
    }
}

impl KernelAdapter for BashKernelAdapter {
    fn supported_languages(&self) -> &[Language] {
        self.inner.supported_languages()
    }

    fn start(&mut self) -> Result<()> {
        self.inner.start()
    }

    fn execute(&mut self, request: &ExecutionRequest) -> Result<KernelExecution> {
        self.inner.execute(request)
    }

    fn interrupt(&mut self) -> Result<()> {
        self.inner.interrupt()
    }

    fn restart(&mut self) -> Result<()> {
        self.inner.restart()
    }

    fn shutdown(&mut self) -> Result<()> {
        self.inner.shutdown()
    }
}

pub struct PythonKernelAdapter {
    inner: WorkerKernelAdapter,
}

impl Default for PythonKernelAdapter {
    fn default() -> Self {
        Self {
            inner: WorkerKernelAdapter::new(
                vec![Language::Python],
                "python3",
                vec![kernel_script("scripts/python_kernel.py")],
            ),
        }
    }
}

impl KernelAdapter for PythonKernelAdapter {
    fn supported_languages(&self) -> &[Language] {
        self.inner.supported_languages()
    }

    fn start(&mut self) -> Result<()> {
        self.inner.start()
    }

    fn execute(&mut self, request: &ExecutionRequest) -> Result<KernelExecution> {
        self.inner.execute(request)
    }

    fn interrupt(&mut self) -> Result<()> {
        self.inner.interrupt()
    }

    fn restart(&mut self) -> Result<()> {
        self.inner.restart()
    }

    fn shutdown(&mut self) -> Result<()> {
        self.inner.shutdown()
    }
}

pub struct JavaScriptKernelAdapter {
    inner: WorkerKernelAdapter,
}

impl Default for JavaScriptKernelAdapter {
    fn default() -> Self {
        let runtime = resolve_js_runtime().unwrap_or_else(|_| JsRuntime {
            program: "node".to_string(),
            args: Vec::new(),
        });
        let mut args = runtime.args;
        args.push(kernel_script("scripts/js_kernel.mjs"));
        Self {
            inner: WorkerKernelAdapter::new(
                vec![Language::JavaScript, Language::TypeScript],
                runtime.program,
                args,
            ),
        }
    }
}

impl KernelAdapter for JavaScriptKernelAdapter {
    fn supported_languages(&self) -> &[Language] {
        self.inner.supported_languages()
    }

    fn start(&mut self) -> Result<()> {
        self.inner.start()
    }

    fn execute(&mut self, request: &ExecutionRequest) -> Result<KernelExecution> {
        self.inner.execute(request)
    }

    fn interrupt(&mut self) -> Result<()> {
        self.inner.interrupt()
    }

    fn restart(&mut self) -> Result<()> {
        self.inner.restart()
    }

    fn shutdown(&mut self) -> Result<()> {
        self.inner.shutdown()
    }
}

struct JsRuntime {
    program: String,
    args: Vec<String>,
}

fn resolve_js_runtime() -> Result<JsRuntime> {
    if command_exists("bun") {
        return Ok(JsRuntime {
            program: "bun".to_string(),
            args: vec!["run".to_string()],
        });
    }
    if command_exists("node") {
        return Ok(JsRuntime {
            program: "node".to_string(),
            args: Vec::new(),
        });
    }
    bail!("neither bun nor node is available on PATH")
}

fn command_exists(program: &str) -> bool {
    let Some(path_os) = std::env::var_os("PATH") else {
        return false;
    };

    std::env::split_paths(&path_os).any(|dir| {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            return ["exe", "cmd", "bat"]
                .iter()
                .map(|ext| dir.join(format!("{program}.{ext}")))
                .any(|path| path.is_file());
        }
        #[cfg(not(windows))]
        {
            false
        }
    })
}

fn kernel_script(relative: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(relative)
        .display()
        .to_string()
}

fn apply_bridges(named_values: &mut BTreeMap<String, String>, bridges: &[BridgeValue]) {
    for bridge in bridges {
        if let BridgeValue::NamedValue { name, value } = bridge {
            named_values.insert(name.clone(), value.clone());
        }
    }
}

pub fn load_session_for_notebook(path: &Path, notebook: &Notebook) -> Result<SessionManager> {
    let checkpoint_paths = crate::storage::CheckpointPaths::for_notebook(path);
    let manifest = if crate::storage::CheckpointStorage::exists(&checkpoint_paths) {
        crate::storage::CheckpointStorage::load(&checkpoint_paths)?
    } else {
        SessionManifest::new(notebook)
    };
    let mut session =
        SessionManager::from_manifest(manifest).with_ai_runtime(AiRuntime::from_env()?);
    session.register_default_kernels()?;
    session.hydrate()?;
    Ok(session)
}

pub fn run_notebook_cells(
    session: &mut SessionManager,
    notebook: &Notebook,
) -> Result<Vec<ExecutionRecord>> {
    let mut records = Vec::new();
    for index in 0..notebook.cells.len() {
        if notebook.cells[index].kind != CellKind::Text {
            records.push(session.run_cell_at(notebook, index)?);
        }
    }
    Ok(records)
}

pub fn summarize_records(records: &[ExecutionRecord]) -> String {
    let mut lines = Vec::new();
    for record in records {
        lines.push(format!(
            "[{}] {} -> {:?} (exit {})",
            record.language.fence_name(),
            record.cell_id,
            record.status,
            record.exit_code
        ));
        if !record.output.is_empty() {
            lines.push(record.output.clone());
        }
        if !record.error_output.is_empty() {
            lines.push(record.error_output.clone());
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Cell, Notebook};

    #[test]
    fn python_kernel_persists_assignments_across_cells() {
        let notebook = Notebook::new("Stateful");
        let mut session = SessionManager::new(&notebook);
        session
            .register_kernel(Box::new(PythonKernelAdapter::default()))
            .unwrap();

        let assign = Cell::code(Language::Python, "value = 42");
        session.run_code_cell(&assign).unwrap();

        let print = Cell::code(Language::Python, "print(value)");
        let record = session.run_code_cell(&print).unwrap();

        assert_eq!(record.output, "42");
    }

    #[test]
    fn bash_kernel_persists_env_across_cells() {
        let notebook = Notebook::new("Shell");
        let mut session = SessionManager::new(&notebook);
        session
            .register_kernel(Box::new(BashKernelAdapter::default()))
            .unwrap();

        let export = Cell::code(Language::Bash, "export NAME=strata");
        session.run_code_cell(&export).unwrap();

        let echo = Cell::code(Language::Bash, "echo $NAME");
        let record = session.run_code_cell(&echo).unwrap();

        assert_eq!(record.output, "strata");
    }

    #[test]
    fn javascript_kernel_persists_state_across_cells() {
        let notebook = Notebook::new("JS");
        let mut session = SessionManager::new(&notebook);
        session
            .register_kernel(Box::new(JavaScriptKernelAdapter::default()))
            .unwrap();

        session
            .run_code_cell(&Cell::code(Language::JavaScript, "globalThis.count = 3;"))
            .unwrap();

        let record = session
            .run_code_cell(&Cell::code(
                Language::JavaScript,
                "globalThis.count += 2; console.log(globalThis.count);",
            ))
            .unwrap();

        assert_eq!(record.output, "5");
    }

    #[test]
    fn hydration_replays_prior_successful_cells() {
        let notebook = Notebook::new("Hydrate").with_cells(vec![
            Cell::code(Language::Python, "value = 42"),
            Cell::code(Language::Python, "print(value)"),
        ]);
        let mut first = SessionManager::new(&notebook);
        first
            .register_kernel(Box::new(PythonKernelAdapter::default()))
            .unwrap();
        first.run_code_cell(&notebook.cells[0]).unwrap();
        let manifest = first.manifest.clone();

        let mut second = SessionManager::from_manifest(manifest);
        second
            .register_kernel(Box::new(PythonKernelAdapter::default()))
            .unwrap();
        second.hydrate().unwrap();
        let record = second.run_code_cell(&notebook.cells[1]).unwrap();

        assert_eq!(record.output, "42");
    }
}
