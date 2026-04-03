use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

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
    Text,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NotebookMetadata {
    pub title: String,
    pub description: Option<String>,
}

impl Default for NotebookMetadata {
    fn default() -> Self {
        Self {
            title: "Untitled Strata Notebook".to_string(),
            description: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub id: CellId,
    pub kind: CellKind,
    pub language: Language,
    pub source: String,
}

impl Cell {
    pub fn text(source: impl Into<String>) -> Self {
        Self {
            id: CellId::new(),
            kind: CellKind::Text,
            language: Language::Text,
            source: source.into(),
        }
    }

    pub fn code(language: Language, source: impl Into<String>) -> Self {
        Self {
            id: CellId::new(),
            kind: CellKind::Code,
            language,
            source: source.into(),
        }
    }

    pub fn ai(source: impl Into<String>) -> Self {
        Self {
            id: CellId::new(),
            kind: CellKind::Ai,
            language: Language::Ai,
            source: source.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Notebook {
    pub metadata: NotebookMetadata,
    pub cells: Vec<Cell>,
}

impl Notebook {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            metadata: NotebookMetadata {
                title: title.into(),
                description: None,
            },
            cells: Vec::new(),
        }
    }

    pub fn with_cells(mut self, cells: Vec<Cell>) -> Self {
        self.cells = cells;
        self
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
pub struct SessionManifest {
    pub session_id: SessionId,
    pub notebook_title: String,
    pub named_values: BTreeMap<String, String>,
    pub ai_history: Vec<AiRunRecord>,
    pub execution_history: Vec<ExecutionRecord>,
    pub artifacts: Vec<ArtifactRef>,
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
        }
    }
}
