use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::core::{
    ArtifactRef, BridgeValue, Cell, ExecutionId, ExecutionRecord, ExecutionRequest,
    ExecutionStatus, Language, Notebook, SessionId, SessionManifest,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelExecution {
    pub output: String,
    pub bridges: Vec<BridgeValue>,
    pub artifacts: Vec<ArtifactRef>,
}

pub trait KernelAdapter: Send {
    fn language(&self) -> Language;
    fn start(&mut self) -> Result<()>;
    fn execute(&mut self, request: &ExecutionRequest) -> Result<KernelExecution>;
    fn interrupt(&mut self) -> Result<()>;
    fn restart(&mut self) -> Result<()>;
    fn hydrate(&mut self, manifest: &SessionManifest) -> Result<()>;
    fn shutdown(&mut self) -> Result<()>;
}

pub struct SessionManager {
    pub session_id: SessionId,
    kernels: HashMap<Language, Box<dyn KernelAdapter>>,
    pub manifest: SessionManifest,
}

impl SessionManager {
    pub fn new(notebook: &Notebook) -> Self {
        Self {
            session_id: SessionId::new(),
            kernels: HashMap::new(),
            manifest: SessionManifest::new(notebook),
        }
    }

    pub fn register_kernel(&mut self, mut kernel: Box<dyn KernelAdapter>) -> Result<()> {
        kernel.start()?;
        self.kernels.insert(kernel.language(), kernel);
        Ok(())
    }

    pub fn hydrate(&mut self) -> Result<()> {
        for kernel in self.kernels.values_mut() {
            kernel.hydrate(&self.manifest)?;
        }
        Ok(())
    }

    pub fn run_cell(&mut self, cell: &Cell) -> Result<ExecutionRecord> {
        let request = ExecutionRequest {
            cell_id: cell.id.clone(),
            language: cell.language,
            source: cell.source.clone(),
        };
        let Some(kernel) = self.kernels.get_mut(&cell.language) else {
            bail!("no kernel registered for {:?}", cell.language);
        };
        let execution = kernel.execute(&request)?;
        let record = ExecutionRecord {
            id: ExecutionId::new(),
            cell_id: cell.id.clone(),
            status: ExecutionStatus::Succeeded,
            output: execution.output,
            dependencies: execution.artifacts.clone(),
            bridges: execution.bridges,
        };
        self.manifest.artifacts.extend(execution.artifacts);
        self.manifest.execution_history.push(record.clone());
        Ok(record)
    }

    pub fn shutdown(&mut self) -> Result<()> {
        for kernel in self.kernels.values_mut() {
            kernel.shutdown()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct BashKernelAdapter {
    cwd: PathBuf,
    env: HashMap<String, String>,
    started: bool,
}

impl Default for BashKernelAdapter {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            env: HashMap::new(),
            started: false,
        }
    }
}

impl KernelAdapter for BashKernelAdapter {
    fn language(&self) -> Language {
        Language::Bash
    }

    fn start(&mut self) -> Result<()> {
        self.started = true;
        Ok(())
    }

    fn execute(&mut self, request: &ExecutionRequest) -> Result<KernelExecution> {
        if !self.started {
            bail!("bash kernel not started");
        }

        let mut output = Vec::new();
        let mut bridges = Vec::new();

        for line in request
            .source
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            if let Some(rest) = line.strip_prefix("export ") {
                if let Some((key, value)) = rest.split_once('=') {
                    let value = value.trim_matches('"').to_string();
                    self.env.insert(key.to_string(), value.clone());
                    bridges.push(BridgeValue::Environment {
                        key: key.to_string(),
                        value,
                    });
                }
                continue;
            }

            if let Some(target) = line.strip_prefix("cd ") {
                let next = if PathBuf::from(target).is_absolute() {
                    PathBuf::from(target)
                } else {
                    self.cwd.join(target)
                };
                self.cwd = next;
                continue;
            }

            if line == "pwd" {
                output.push(self.cwd.display().to_string());
                continue;
            }

            if let Some(rest) = line.strip_prefix("echo ") {
                let mut rendered = rest.to_string();
                for (key, value) in &self.env {
                    rendered = rendered.replace(&format!("${key}"), value);
                }
                output.push(rendered.trim_matches('"').to_string());
                bridges.push(BridgeValue::Stdout(rendered.trim_matches('"').to_string()));
                continue;
            }

            output.push(format!("unhandled bash: {line}"));
        }

        Ok(KernelExecution {
            output: output.join("\n"),
            bridges,
            artifacts: Vec::new(),
        })
    }

    fn interrupt(&mut self) -> Result<()> {
        Ok(())
    }

    fn restart(&mut self) -> Result<()> {
        self.env.clear();
        self.cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Ok(())
    }

    fn hydrate(&mut self, manifest: &SessionManifest) -> Result<()> {
        for record in &manifest.execution_history {
            for bridge in &record.bridges {
                if let BridgeValue::Environment { key, value } = bridge {
                    self.env.insert(key.clone(), value.clone());
                }
            }
        }
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        self.started = false;
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct PythonKernelAdapter {
    variables: HashMap<String, String>,
    started: bool,
}

impl KernelAdapter for PythonKernelAdapter {
    fn language(&self) -> Language {
        Language::Python
    }

    fn start(&mut self) -> Result<()> {
        self.started = true;
        Ok(())
    }

    fn execute(&mut self, request: &ExecutionRequest) -> Result<KernelExecution> {
        if !self.started {
            bail!("python kernel not started");
        }

        let mut output = Vec::new();
        let mut bridges = Vec::new();

        for line in request
            .source
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            if let Some((name, value)) = line.split_once('=') {
                let name = name.trim();
                let value = value.trim().trim_matches('"').to_string();
                self.variables.insert(name.to_string(), value);
                continue;
            }

            if line.starts_with("print(") && line.ends_with(')') {
                let inner = line
                    .trim_start_matches("print(")
                    .trim_end_matches(')')
                    .trim()
                    .trim_matches('"');
                let rendered = self
                    .variables
                    .get(inner)
                    .cloned()
                    .unwrap_or_else(|| inner.to_string());
                bridges.push(BridgeValue::Stdout(rendered.clone()));
                output.push(rendered);
                continue;
            }

            if let Some(value) = self.variables.get(line) {
                output.push(value.clone());
                continue;
            }

            output.push(format!("unhandled python: {line}"));
        }

        Ok(KernelExecution {
            output: output.join("\n"),
            bridges,
            artifacts: Vec::new(),
        })
    }

    fn interrupt(&mut self) -> Result<()> {
        Ok(())
    }

    fn restart(&mut self) -> Result<()> {
        self.variables.clear();
        Ok(())
    }

    fn hydrate(&mut self, _manifest: &SessionManifest) -> Result<()> {
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        self.started = false;
        Ok(())
    }
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
}
