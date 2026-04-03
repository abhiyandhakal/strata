use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

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
    fn hydrate(&mut self, manifest: &SessionManifest) -> Result<()>;
    fn shutdown(&mut self) -> Result<()>;
}

pub struct SessionManager {
    pub session_id: SessionId,
    kernels: Vec<Box<dyn KernelAdapter>>,
    language_map: HashMap<Language, usize>,
    pub manifest: SessionManifest,
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
        }
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

    pub fn hydrate(&mut self) -> Result<()> {
        for kernel in &mut self.kernels {
            kernel.hydrate(&self.manifest)?;
        }
        Ok(())
    }

    pub fn run_cell(&mut self, cell: &Cell) -> Result<ExecutionRecord> {
        if cell.kind != CellKind::Code {
            bail!("only code cells can be executed by the runtime");
        }

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

        for bridge in &execution.bridges {
            if let BridgeValue::NamedValue { name, value } = bridge {
                self.manifest
                    .named_values
                    .insert(name.clone(), value.clone());
            }
        }

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

    fn hydrate(&mut self, _manifest: &SessionManifest) -> Result<()> {
        Ok(())
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

    fn hydrate(&mut self, manifest: &SessionManifest) -> Result<()> {
        self.inner.hydrate(manifest)
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

    fn hydrate(&mut self, manifest: &SessionManifest) -> Result<()> {
        self.inner.hydrate(manifest)
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

    fn hydrate(&mut self, manifest: &SessionManifest) -> Result<()> {
        self.inner.hydrate(manifest)
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

pub fn run_notebook_cells(
    session: &mut SessionManager,
    notebook: &Notebook,
) -> Result<Vec<ExecutionRecord>> {
    let mut records = Vec::new();
    for cell in &notebook.cells {
        if cell.kind == CellKind::Code {
            records.push(session.run_cell(cell)?);
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
    use crate::core::{Cell, Language, Notebook};

    #[test]
    fn python_kernel_persists_assignments_across_cells() {
        let notebook = Notebook::new("Stateful");
        let mut session = SessionManager::new(&notebook);
        session
            .register_kernel(Box::new(PythonKernelAdapter::default()))
            .unwrap();

        let assign = Cell::code(Language::Python, "value = 42");
        session.run_cell(&assign).unwrap();

        let print = Cell::code(Language::Python, "print(value)");
        let record = session.run_cell(&print).unwrap();

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
        session.run_cell(&export).unwrap();

        let echo = Cell::code(Language::Bash, "echo $NAME");
        let record = session.run_cell(&echo).unwrap();

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
            .run_cell(&Cell::code(Language::JavaScript, "globalThis.count = 3;"))
            .unwrap();

        let record = session
            .run_cell(&Cell::code(
                Language::JavaScript,
                "globalThis.count += 2; console.log(globalThis.count);",
            ))
            .unwrap();

        assert_eq!(record.output, "5");
    }

    #[test]
    fn named_values_flow_across_languages() {
        let notebook = Notebook::new("Flow");
        let mut session = SessionManager::new(&notebook);
        session.register_default_kernels().unwrap();

        let python = session
            .run_cell(&Cell::code(
                Language::Python,
                "strata.export('shared', 'hello')",
            ))
            .unwrap();
        let bash = session
            .run_cell(&Cell::code(Language::Bash, "echo $(strata_input shared)"))
            .unwrap();

        assert_eq!(python.status, ExecutionStatus::Succeeded);
        assert_eq!(bash.output, "hello");
        assert_eq!(
            session.manifest.named_values.get("shared"),
            Some(&"hello".to_string())
        );
    }

    #[test]
    fn failed_python_cell_records_stderr_and_status() {
        let notebook = Notebook::new("Failure");
        let mut session = SessionManager::new(&notebook);
        session
            .register_kernel(Box::new(PythonKernelAdapter::default()))
            .unwrap();

        let record = session
            .run_cell(&Cell::code(Language::Python, "raise RuntimeError('boom')"))
            .unwrap();

        assert_eq!(record.status, ExecutionStatus::Failed);
        assert_ne!(record.exit_code, 0);
        assert!(record.error_output.contains("RuntimeError"));
    }
}
