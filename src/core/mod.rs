use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

static CELL_COUNTER: AtomicU64 = AtomicU64::new(1);
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);
static EXECUTION_COUNTER: AtomicU64 = AtomicU64::new(1);
static ARTIFACT_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id(prefix: &str, counter: &AtomicU64) -> String {
    let id = counter.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{id:04}")
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct CellId(pub String);

impl CellId {
    pub fn new() -> Self {
        Self(next_id("cell", &CELL_COUNTER))
    }
}

impl Default for CellId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for CellId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new() -> Self {
        Self(next_id("session", &SESSION_COUNTER))
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ExecutionId(pub String);

impl ExecutionId {
    pub fn new() -> Self {
        Self(next_id("exec", &EXECUTION_COUNTER))
    }
}

impl Default for ExecutionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ArtifactId(pub String);

impl ArtifactId {
    pub fn new() -> Self {
        Self(next_id("artifact", &ARTIFACT_COUNTER))
    }
}

impl Default for ArtifactId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    Code,
    Markdown,
    Raw,
    Ai,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Bash,
    Python,
    JavaScript,
    TypeScript,
    Text,
    Ai,
}

impl Language {
    pub fn fence_name(self) -> &'static str {
        match self {
            Language::Bash => "bash",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Text => "text",
            Language::Ai => "ai",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelKind {
    Python,
    Bash,
    JavaScript,
}

impl KernelKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::Bash => "Bash",
            Self::JavaScript => "JavaScript",
        }
    }

    pub fn kernelspec(self) -> Kernelspec {
        match self {
            Self::Python => Kernelspec {
                display_name: "Python 3".to_string(),
                language: "python".to_string(),
                name: "python3".to_string(),
            },
            Self::Bash => Kernelspec {
                display_name: "Bash".to_string(),
                language: "bash".to_string(),
                name: "bash".to_string(),
            },
            Self::JavaScript => Kernelspec {
                display_name: "JavaScript".to_string(),
                language: "javascript".to_string(),
                name: "javascript".to_string(),
            },
        }
    }

    pub fn language_info(self) -> LanguageInfo {
        match self {
            Self::Python => LanguageInfo::default(),
            Self::Bash => LanguageInfo {
                name: "bash".to_string(),
                version: None,
                mimetype: Some("application/x-sh".to_string()),
                file_extension: Some(".sh".to_string()),
            },
            Self::JavaScript => LanguageInfo {
                name: "javascript".to_string(),
                version: None,
                mimetype: Some("application/javascript".to_string()),
                file_extension: Some(".js".to_string()),
            },
        }
    }

    pub fn language(self) -> Language {
        match self {
            Self::Python => Language::Python,
            Self::Bash => Language::Bash,
            Self::JavaScript => Language::JavaScript,
        }
    }
}

impl Default for KernelKind {
    fn default() -> Self {
        Self::Python
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NotebookRuntime {
    #[serde(default)]
    pub kernel: KernelKind,
    #[serde(default)]
    pub environment: String,
}

impl Default for NotebookRuntime {
    fn default() -> Self {
        Self {
            kernel: KernelKind::Python,
            environment: "system".to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Kernelspec {
    pub display_name: String,
    pub language: String,
    pub name: String,
}

impl Default for Kernelspec {
    fn default() -> Self {
        Self {
            display_name: "Python 3".to_string(),
            language: "python".to_string(),
            name: "python3".to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LanguageInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mimetype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_extension: Option<String>,
}

impl Default for LanguageInfo {
    fn default() -> Self {
        Self {
            name: "python".to_string(),
            version: None,
            mimetype: Some("text/x-python".to_string()),
            file_extension: Some(".py".to_string()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NotebookMetadata {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub kernelspec: Kernelspec,
    #[serde(default)]
    pub language_info: LanguageInfo,
    #[serde(default)]
    pub runtime: NotebookRuntime,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl Default for NotebookMetadata {
    fn default() -> Self {
        Self {
            title: "Untitled Notebook".to_string(),
            description: None,
            kernelspec: Kernelspec::default(),
            language_info: LanguageInfo::default(),
            runtime: NotebookRuntime::default(),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "output_type", rename_all = "snake_case")]
pub enum CellOutput {
    Stream { name: String, text: String },
    ExecuteResult {
        execution_count: u32,
        data: BTreeMap<String, Value>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
    },
    DisplayData {
        data: BTreeMap<String, Value>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
    },
    Error {
        ename: String,
        evalue: String,
        traceback: Vec<String>,
    },
}

impl CellOutput {
    pub fn as_text(&self) -> String {
        match self {
            CellOutput::Stream { text, .. } => text.clone(),
            CellOutput::ExecuteResult { data, .. } | CellOutput::DisplayData { data, .. } => data
                .get("text/plain")
                .map(render_json_value)
                .unwrap_or_default(),
            CellOutput::Error {
                ename,
                evalue,
                traceback,
            } => {
                if traceback.is_empty() {
                    format!("{ename}: {evalue}")
                } else {
                    traceback.join("\n")
                }
            }
        }
    }

    pub fn image_info(&self) -> Option<ImageOutput> {
        let (data, metadata, execution_count) = match self {
            CellOutput::ExecuteResult {
                data,
                metadata,
                execution_count,
            } => (data, metadata, Some(*execution_count)),
            CellOutput::DisplayData { data, metadata } => (data, metadata, None),
            _ => return None,
        };

        let mime = ["image/png", "image/jpeg", "image/svg+xml", "image/gif"]
            .into_iter()
            .find(|mime| data.contains_key(*mime))
            .map(str::to_string)
            .or_else(|| {
                metadata
                    .get("strata_image_mime")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })?;

        let data_value = data.get(&mime).cloned();
        let path = metadata
            .get("strata_image_path")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let alt = data
            .get("text/plain")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| {
                metadata
                    .get("strata_image_alt")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            });

        Some(ImageOutput {
            mime,
            data: data_value,
            path,
            alt,
            execution_count,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageOutput {
    pub mime: String,
    pub data: Option<Value>,
    pub path: Option<String>,
    pub alt: Option<String>,
    pub execution_count: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub id: CellId,
    pub kind: CellKind,
    pub language: Language,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<CellOutput>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

impl Cell {
    pub fn markdown(source: impl Into<String>) -> Self {
        Self {
            id: CellId::new(),
            kind: CellKind::Markdown,
            language: Language::Text,
            source: source.into(),
            execution_count: None,
            outputs: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    pub fn text(source: impl Into<String>) -> Self {
        Self::markdown(source)
    }

    pub fn raw(source: impl Into<String>) -> Self {
        Self {
            id: CellId::new(),
            kind: CellKind::Raw,
            language: Language::Text,
            source: source.into(),
            execution_count: None,
            outputs: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    pub fn code(language: Language, source: impl Into<String>) -> Self {
        Self {
            id: CellId::new(),
            kind: CellKind::Code,
            language,
            source: source.into(),
            execution_count: None,
            outputs: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    pub fn ai(source: impl Into<String>) -> Self {
        Self {
            id: CellId::new(),
            kind: CellKind::Ai,
            language: Language::Ai,
            source: source.into(),
            execution_count: None,
            outputs: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    pub fn primary_output_text(&self) -> String {
        self.outputs
            .iter()
            .map(CellOutput::as_text)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Notebook {
    pub metadata: NotebookMetadata,
    pub nbformat: u8,
    pub nbformat_minor: u8,
    pub cells: Vec<Cell>,
}

impl Notebook {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            metadata: NotebookMetadata {
                title: title.into(),
                ..NotebookMetadata::default()
            },
            nbformat: 4,
            nbformat_minor: 5,
            cells: Vec::new(),
        }
    }

    pub fn with_cells(mut self, cells: Vec<Cell>) -> Self {
        self.cells = cells;
        self
    }

    pub fn display_title(&self, path: Option<&Path>) -> String {
        if self.metadata.title == "Untitled Notebook" {
            if let Some(path) = path {
                if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                    return stem.to_string();
                }
            }
        }
        self.metadata.title.clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub id: ArtifactId,
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextBundle {
    pub summary: String,
    pub cell_ids: Vec<String>,
    pub snippets: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum BridgeValue {
    Stdout(String),
    Environment { key: String, value: String },
    NamedValue { name: String, value: String },
    Artifact(ArtifactRef),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub cell_id: CellId,
    pub language: Language,
    pub source: String,
    pub named_values: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Pending,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub id: ExecutionId,
    pub cell_id: CellId,
    pub language: Language,
    pub source: String,
    pub status: ExecutionStatus,
    pub output: String,
    pub error_output: String,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<CellOutput>,
    pub dependencies: Vec<ArtifactRef>,
    pub bridges: Vec<BridgeValue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AiRunRecord {
    pub prompt_cell_id: String,
    pub prompt: String,
    pub context: ContextBundle,
    pub provider_name: String,
    pub model_id: String,
    pub response: String,
    pub error_output: String,
    pub status: ExecutionStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UiState {
    pub selected_cell: usize,
    pub viewport_row: usize,
    pub cell_modes: BTreeMap<String, CellUiState>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            selected_cell: 0,
            viewport_row: 0,
            cell_modes: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CellUiState {
    pub rendered: bool,
    #[serde(default)]
    pub body_collapsed: bool,
    pub output_collapsed: bool,
}

impl Default for CellUiState {
    fn default() -> Self {
        Self {
            rendered: true,
            body_collapsed: false,
            output_collapsed: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionManifest {
    pub session_id: SessionId,
    pub notebook_title: String,
    pub named_values: BTreeMap<String, String>,
    pub ai_history: Vec<AiRunRecord>,
    pub execution_history: Vec<ExecutionRecord>,
    pub artifacts: Vec<ArtifactRef>,
    pub next_execution_count: u32,
    #[serde(default)]
    pub ui_state: UiState,
}

impl SessionManifest {
    pub fn new(notebook: &Notebook) -> Self {
        Self {
            session_id: SessionId::new(),
            notebook_title: notebook.metadata.title.clone(),
            named_values: BTreeMap::new(),
            ai_history: Vec::new(),
            execution_history: Vec::new(),
            artifacts: Vec::new(),
            next_execution_count: 1,
            ui_state: UiState::default(),
        }
    }
}

fn render_json_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn untitled_notebook_uses_file_stem_for_display_title() {
        let notebook = Notebook::new("Untitled Notebook");

        assert_eq!(
            notebook.display_title(Some(Path::new("/tmp/report.smd"))),
            "report"
        );
    }
}
