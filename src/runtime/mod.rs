use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::ai::AiRuntime;
use crate::core::{
    ArtifactId, ArtifactRef, BridgeValue, Cell, CellKind, CellOutput, ExecutionId, ExecutionRecord,
    ExecutionRequest, ExecutionStatus, KernelKind, Language, Notebook, SessionId, SessionManifest,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentKind {
    None,
    System,
    PythonInterpreter(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentOption {
    pub id: String,
    pub label: String,
    pub kind: EnvironmentKind,
}

impl EnvironmentOption {
    pub fn none() -> Self {
        Self {
            id: "none".to_string(),
            label: "None".to_string(),
            kind: EnvironmentKind::None,
        }
    }

    pub fn system() -> Self {
        Self {
            id: "system".to_string(),
            label: "System".to_string(),
            kind: EnvironmentKind::System,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelExecution {
    pub output: String,
    pub error_output: String,
    pub exit_code: i32,
    pub bridges: Vec<BridgeValue>,
    pub artifacts: Vec<ArtifactRef>,
    pub displays: Vec<KernelDisplay>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelDisplay {
    pub data: BTreeMap<String, serde_json::Value>,
    pub metadata: BTreeMap<String, serde_json::Value>,
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

    pub fn configure_for_notebook(
        &mut self,
        notebook: &Notebook,
        notebook_path: Option<&Path>,
    ) -> Result<()> {
        self.shutdown()?;
        self.kernels.clear();
        self.language_map.clear();

        if is_legacy_multikernel_notebook(notebook) {
            self.register_default_kernels()?;
            self.hydrate()?;
            return Ok(());
        }

        match notebook.metadata.runtime.kernel {
            KernelKind::Python => {
                let environments = discover_environments(notebook_path, KernelKind::Python);
                let selected = environments
                    .iter()
                    .find(|environment| environment.id == notebook.metadata.runtime.environment)
                    .cloned()
                    .unwrap_or_else(EnvironmentOption::system);
                match selected.kind {
                    EnvironmentKind::None => {}
                    EnvironmentKind::System => {
                        self.register_kernel(Box::new(PythonKernelAdapter::default()))?;
                    }
                    EnvironmentKind::PythonInterpreter(path) => {
                        self.register_kernel(Box::new(PythonKernelAdapter::from_interpreter(
                            path,
                        )))?;
                    }
                }
            }
            KernelKind::Bash => {
                if notebook.metadata.runtime.environment != "none" {
                    self.register_kernel(Box::new(BashKernelAdapter::default()))?;
                }
            }
            KernelKind::JavaScript => {
                if notebook.metadata.runtime.environment != "none" {
                    self.register_kernel(Box::new(JavaScriptKernelAdapter::default()))?;
                }
            }
        }
        self.hydrate()?;
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

    pub fn run_cell_at(
        &mut self,
        notebook: &mut Notebook,
        index: usize,
    ) -> Result<ExecutionRecord> {
        let cell = notebook
            .cells
            .get(index)
            .context("cell index out of bounds")?;
        let record = match cell.kind {
            CellKind::Code => self.run_code_cell(cell)?,
            CellKind::Ai => self.run_ai_cell(notebook, index)?,
            CellKind::Markdown | CellKind::Raw => bail!("selected cell is not executable"),
        };
        apply_record_to_notebook(notebook, index, &record);
        Ok(record)
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

        let execution_count = self.manifest.next_execution_count;
        self.manifest.next_execution_count += 1;
        let outputs = build_cell_outputs(execution_count, &execution);
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
            execution_count: Some(execution_count),
            outputs,
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
            execution_count: None,
            outputs: if ai_run.response.is_empty() {
                Vec::new()
            } else {
                vec![CellOutput::Stream {
                    name: "stdout".to_string(),
                    text: ai_run.response.clone(),
                }]
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

    pub fn restart_all(&mut self) -> Result<()> {
        for kernel in &mut self.kernels {
            kernel.restart()?;
        }
        self.manifest.named_values.clear();
        self.manifest.next_execution_count = 1;
        Ok(())
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
    #[serde(default)]
    displays: Vec<WorkerDisplay>,
}

#[derive(Debug, Deserialize)]
struct WorkerDisplay {
    #[serde(default)]
    data: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    metadata: BTreeMap<String, serde_json::Value>,
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
            displays: response
                .displays
                .into_iter()
                .map(|display| KernelDisplay {
                    data: display.data,
                    metadata: display.metadata,
                })
                .collect(),
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
        Self::from_program("python3")
    }
}

impl PythonKernelAdapter {
    fn from_program(program: impl Into<String>) -> Self {
        Self {
            inner: WorkerKernelAdapter::new(
                vec![Language::Python],
                program.into(),
                vec![kernel_script("scripts/python_kernel.py")],
            ),
        }
    }

    pub fn from_interpreter(path: PathBuf) -> Self {
        Self::from_program(path.display().to_string())
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

fn is_legacy_multikernel_notebook(notebook: &Notebook) -> bool {
    notebook.metadata.runtime == crate::core::NotebookRuntime::default()
        && notebook
            .cells
            .iter()
            .any(|cell| matches!(cell.kind, CellKind::Code) && cell.language != Language::Python)
}

pub fn discover_environments(
    notebook_path: Option<&Path>,
    kernel: KernelKind,
) -> Vec<EnvironmentOption> {
    let mut environments = vec![EnvironmentOption::none(), EnvironmentOption::system()];
    if kernel != KernelKind::Python {
        return environments;
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut push_python = |label: String, path: PathBuf| {
        if !path.is_file() {
            return;
        }
        let id = path.display().to_string();
        if seen.insert(id.clone()) {
            environments.push(EnvironmentOption {
                id,
                label,
                kind: EnvironmentKind::PythonInterpreter(path),
            });
        }
    };

    if let Ok(path) = std::env::var("VIRTUAL_ENV") {
        push_python(
            "Active venv".to_string(),
            PathBuf::from(path).join("bin").join("python"),
        );
    }
    if let Ok(path) = std::env::var("CONDA_PREFIX") {
        push_python(
            "Active conda".to_string(),
            PathBuf::from(path).join("bin").join("python"),
        );
    }
    if let Some(parent) = notebook_path.and_then(Path::parent) {
        for name in [".venv", "venv", "env"] {
            push_python(
                format!(
                    "{} ({name})",
                    parent
                        .file_name()
                        .and_then(|v| v.to_str())
                        .unwrap_or("Project")
                ),
                parent.join(name).join("bin").join("python"),
            );
        }
    }
    environments
}

fn apply_bridges(named_values: &mut BTreeMap<String, String>, bridges: &[BridgeValue]) {
    for bridge in bridges {
        if let BridgeValue::NamedValue { name, value } = bridge {
            named_values.insert(name.clone(), value.clone());
        }
    }
}

pub fn load_session_for_notebook(
    path: &Path,
    notebook: &mut Notebook,
) -> Result<(SessionManager, Option<String>)> {
    let checkpoint_paths = crate::storage::CheckpointPaths::for_notebook(path);
    let (manifest, notice) = if crate::storage::CheckpointStorage::exists(&checkpoint_paths) {
        let manifest = crate::storage::CheckpointStorage::load(&checkpoint_paths)?;
        reconcile_manifest_with_notebook(notebook, manifest)
    } else {
        (SessionManifest::new(notebook), None)
    };
    let mut session =
        SessionManager::from_manifest(manifest).with_ai_runtime(AiRuntime::from_env()?);
    session.configure_for_notebook(notebook, Some(path))?;
    Ok((session, notice))
}

pub fn run_notebook_cells(
    session: &mut SessionManager,
    notebook: &mut Notebook,
) -> Result<Vec<ExecutionRecord>> {
    let mut records = Vec::new();
    for index in 0..notebook.cells.len() {
        if matches!(notebook.cells[index].kind, CellKind::Code | CellKind::Ai) {
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

fn build_cell_outputs(execution_count: u32, execution: &KernelExecution) -> Vec<CellOutput> {
    let mut outputs = Vec::new();
    outputs.extend(build_display_outputs(&execution.displays));
    if let Some(image_output) = build_image_output(execution_count, execution) {
        outputs.push(image_output);
    }
    if !execution.output.is_empty() {
        outputs.push(CellOutput::Stream {
            name: "stdout".to_string(),
            text: execution.output.clone(),
        });
        outputs.push(CellOutput::ExecuteResult {
            execution_count,
            data: BTreeMap::from([(
                "text/plain".to_string(),
                serde_json::Value::String(execution.output.clone()),
            )]),
            metadata: BTreeMap::new(),
        });
    }
    if !execution.error_output.is_empty() {
        outputs.push(CellOutput::Error {
            ename: "ExecutionError".to_string(),
            evalue: execution.error_output.clone(),
            traceback: execution
                .error_output
                .lines()
                .map(ToString::to_string)
                .collect(),
        });
    }
    outputs
}

fn build_display_outputs(displays: &[KernelDisplay]) -> Vec<CellOutput> {
    displays
        .iter()
        .filter(|display| !display.data.is_empty())
        .map(|display| CellOutput::DisplayData {
            data: display.data.clone(),
            metadata: display.metadata.clone(),
        })
        .collect()
}

fn build_image_output(_execution_count: u32, execution: &KernelExecution) -> Option<CellOutput> {
    let output = execution.output.trim();
    let image_path = if let Some(rest) = output.strip_prefix("display ") {
        Some(rest.trim())
    } else if looks_like_image_path(output) {
        Some(output)
    } else {
        None
    }?;

    let path = PathBuf::from(image_path);
    if !path.exists() {
        return None;
    }
    let mime = match path.extension().and_then(|value| value.to_str()) {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("gif") => "image/gif",
        _ => return None,
    };

    let mut data = BTreeMap::new();
    data.insert(
        "text/plain".to_string(),
        serde_json::Value::String(
            path.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("image")
                .to_string(),
        ),
    );
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "strata_image_path".to_string(),
        serde_json::Value::String(path.display().to_string()),
    );
    metadata.insert(
        "strata_image_mime".to_string(),
        serde_json::Value::String(mime.to_string()),
    );
    Some(CellOutput::DisplayData { data, metadata })
}

fn looks_like_image_path(output: &str) -> bool {
    output.ends_with(".png")
        || output.ends_with(".jpg")
        || output.ends_with(".jpeg")
        || output.ends_with(".svg")
        || output.ends_with(".gif")
}

fn apply_record_to_notebook(notebook: &mut Notebook, index: usize, record: &ExecutionRecord) {
    let Some(cell) = notebook.cells.get_mut(index) else {
        return;
    };
    cell.execution_count = record.execution_count;
    cell.outputs = record.outputs.clone();
}

fn reconcile_manifest_with_notebook(
    notebook: &mut Notebook,
    manifest: SessionManifest,
) -> (SessionManifest, Option<String>) {
    let mut manifest = manifest;
    let cell_index_by_id: HashMap<String, usize> = notebook
        .cells
        .iter()
        .enumerate()
        .map(|(index, cell)| (cell.id.0.clone(), index))
        .collect();
    let latest_matching_record_by_cell: HashMap<String, ExecutionRecord> = manifest
        .execution_history
        .iter()
        .rev()
        .filter_map(|record| {
            let cell = cell_index_by_id
                .get(&record.cell_id.0)
                .and_then(|index| notebook.cells.get(*index))?;
            if record_matches_cell(record, cell) {
                Some((record.cell_id.0.clone(), record.clone()))
            } else {
                None
            }
        })
        .collect();

    let mut broken_languages: HashSet<Language> = HashSet::new();
    let mut valid_record_ids = std::collections::BTreeSet::new();
    let mut invalidated_cells = 0usize;

    for cell in &mut notebook.cells {
        let should_track_runtime = matches!(cell.kind, CellKind::Code | CellKind::Ai);
        if !should_track_runtime {
            continue;
        }

        let record = latest_matching_record_by_cell.get(&cell.id.0).cloned();
        let language_broken = broken_languages.contains(&cell.language);
        if language_broken || record.is_none() {
            if record.is_none() {
                invalidated_cells += 1;
            }
            if matches!(cell.kind, CellKind::Code) {
                broken_languages.insert(cell.language);
            }
            clear_cell_runtime_state(cell);
            continue;
        }

        let record = record.expect("record checked above");
        valid_record_ids.insert(record.id.0.clone());
        cell.execution_count = record.execution_count;
        cell.outputs = record.outputs.clone();
    }

    manifest.execution_history.retain(|record| {
        valid_record_ids.contains(&record.id.0) && cell_index_by_id.contains_key(&record.cell_id.0)
    });
    manifest.ai_history.retain(|run| {
        cell_index_by_id.contains_key(&run.prompt_cell_id)
            && notebook
                .cells
                .iter()
                .find(|cell| cell.id.0 == run.prompt_cell_id)
                .map(|cell| cell.kind == CellKind::Ai && cell.source == run.prompt)
                .unwrap_or(false)
    });

    let mut named_values = BTreeMap::new();
    let mut artifacts = Vec::new();
    let mut next_execution_count = 1u32;
    for cell in &notebook.cells {
        if let Some(record) = manifest
            .execution_history
            .iter()
            .find(|record| record.cell_id == cell.id)
        {
            apply_bridges(&mut named_values, &record.bridges);
            artifacts.extend(record.dependencies.clone());
            if let Some(count) = record.execution_count {
                next_execution_count = next_execution_count.max(count + 1);
            }
        }
    }
    manifest.named_values = named_values;
    manifest.artifacts = artifacts;
    manifest.next_execution_count = next_execution_count;
    manifest.ui_state.selected_cell = manifest
        .ui_state
        .selected_cell
        .map(|selected| selected.min(notebook.cells.len().saturating_sub(1)));
    manifest
        .ui_state
        .cell_modes
        .retain(|cell_id, _| cell_index_by_id.contains_key(cell_id));

    let total_current_cells = notebook
        .cells
        .iter()
        .filter(|cell| matches!(cell.kind, CellKind::Code | CellKind::Ai))
        .count();
    let valid_cells = notebook
        .cells
        .iter()
        .filter(|cell| {
            matches!(cell.kind, CellKind::Code | CellKind::Ai)
                && manifest
                    .execution_history
                    .iter()
                    .any(|record| record.cell_id == cell.id)
        })
        .count();
    let dropped = total_current_cells.saturating_sub(valid_cells);
    let notice = if dropped > 0 || invalidated_cells > 0 {
        Some(format!(
            "notebook changed outside Strata; invalidated stale checkpoint state for {} cells",
            dropped.max(invalidated_cells)
        ))
    } else {
        None
    };

    (manifest, notice)
}

fn record_matches_cell(record: &ExecutionRecord, cell: &Cell) -> bool {
    record.cell_id == cell.id && record.language == cell.language && record.source == cell.source
}

fn clear_cell_runtime_state(cell: &mut Cell) {
    cell.execution_count = None;
    cell.outputs.clear();
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
    fn discover_environments_includes_none_and_system() {
        let environments = discover_environments(None, KernelKind::Python);
        assert_eq!(environments[0].id, "none");
        assert_eq!(environments[1].id, "system");
    }

    #[test]
    fn build_image_output_detects_display_command() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let path = temp.path().with_extension("png");
        std::fs::write(&path, b"png").unwrap();
        let execution = KernelExecution {
            output: format!("display {}", path.display()),
            error_output: String::new(),
            exit_code: 0,
            bridges: Vec::new(),
            artifacts: Vec::new(),
            displays: Vec::new(),
        };
        let output = build_image_output(1, &execution);
        assert!(output.unwrap().image_info().is_some());
    }

    #[test]
    fn python_display_creates_display_data_output() {
        let notebook = Notebook::new("Display");
        let mut session = SessionManager::new(&notebook);
        session
            .register_kernel(Box::new(PythonKernelAdapter::default()))
            .unwrap();

        let record = session
            .run_code_cell(&Cell::code(
                Language::Python,
                "display('hello from display')",
            ))
            .unwrap();

        assert!(record.error_output.is_empty());
        assert!(record.outputs.iter().any(|output| matches!(
            output,
            CellOutput::DisplayData { data, .. }
                if data.get("text/plain")
                    == Some(&serde_json::Value::String("hello from display".to_string()))
        )));
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

    #[test]
    fn reconciliation_invalidates_changed_cells_and_downstream_language_state() {
        let original = Notebook::new("Changed").with_cells(vec![
            Cell::code(Language::Python, "value = 42"),
            Cell::code(Language::Python, "print(value)"),
        ]);
        let mut session = SessionManager::new(&original);
        session
            .register_kernel(Box::new(PythonKernelAdapter::default()))
            .unwrap();
        let first = session.run_code_cell(&original.cells[0]).unwrap();
        let second = session.run_code_cell(&original.cells[1]).unwrap();
        assert_eq!(second.output, "42");

        let mut edited = original.clone();
        edited.cells[0].source = "value = 7".to_string();
        edited.cells[0].execution_count = Some(99);
        edited.cells[0].outputs = vec![CellOutput::Stream {
            name: "stdout".to_string(),
            text: "stale".to_string(),
        }];
        edited.cells[1].execution_count = Some(99);
        edited.cells[1].outputs = vec![CellOutput::Stream {
            name: "stdout".to_string(),
            text: "stale".to_string(),
        }];

        let (manifest, notice) = reconcile_manifest_with_notebook(&mut edited, session.manifest);

        assert!(
            notice
                .unwrap()
                .contains("invalidated stale checkpoint state")
        );
        assert!(manifest.execution_history.is_empty());
        assert_eq!(edited.cells[0].execution_count, None);
        assert!(edited.cells[0].outputs.is_empty());
        assert_eq!(edited.cells[1].execution_count, None);
        assert!(edited.cells[1].outputs.is_empty());
        let _ = first;
    }

    #[test]
    fn reconciliation_keeps_matching_unchanged_cells() {
        let notebook = Notebook::new("Keep").with_cells(vec![
            Cell::code(Language::Python, "value = 42"),
            Cell::code(Language::Python, "print(value)"),
        ]);
        let mut session = SessionManager::new(&notebook);
        session
            .register_kernel(Box::new(PythonKernelAdapter::default()))
            .unwrap();
        session.run_code_cell(&notebook.cells[0]).unwrap();
        let second = session.run_code_cell(&notebook.cells[1]).unwrap();

        let mut reopened = notebook.clone();
        let (manifest, notice) = reconcile_manifest_with_notebook(&mut reopened, session.manifest);

        assert!(notice.is_none());
        assert_eq!(manifest.execution_history.len(), 2);
        assert_eq!(reopened.cells[1].execution_count, second.execution_count);
        assert_eq!(reopened.cells[1].outputs, second.outputs);
    }
}
