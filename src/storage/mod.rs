use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::{
    Cell, CellId, CellKind, CellOutput, Kernelspec, Language, LanguageInfo, Notebook,
    NotebookMetadata, SessionManifest,
};

#[derive(Clone, Debug)]
pub struct CheckpointPaths {
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub artifacts: PathBuf,
}

impl CheckpointPaths {
    pub fn for_notebook(path: &Path) -> Self {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("notebook");
        let root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".strata")
            .join(stem);
        Self {
            manifest: root.join("session.json"),
            artifacts: root.join("artifacts"),
            root,
        }
    }
}

pub struct NotebookStorage;

impl NotebookStorage {
    pub fn load(path: &Path) -> Result<Notebook> {
        match path.extension().and_then(|value| value.to_str()) {
            Some("ipynb") => Self::load_ipynb(path),
            _ => Self::load_markdown(path),
        }
    }

    pub fn save(path: &Path, notebook: &Notebook) -> Result<()> {
        match path.extension().and_then(|value| value.to_str()) {
            Some("ipynb") => Self::save_ipynb(path, notebook),
            _ => Self::save_markdown(path, notebook),
        }
    }

    pub fn render(path: Option<&Path>, notebook: &Notebook) -> String {
        match path.and_then(|value| value.extension().and_then(|ext| ext.to_str())) {
            Some("ipynb") => Self::render_ipynb(notebook),
            _ => Self::render_markdown(notebook),
        }
    }

    pub fn load_ipynb(path: &Path) -> Result<Notebook> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read notebook at {}", path.display()))?;
        Self::parse_ipynb(&raw)
    }

    pub fn save_ipynb(path: &Path, notebook: &Notebook) -> Result<()> {
        let rendered = Self::render_ipynb(notebook);
        fs::write(path, rendered)
            .with_context(|| format!("failed to write notebook at {}", path.display()))
    }

    pub fn parse_ipynb(raw: &str) -> Result<Notebook> {
        let parsed: IpynbNotebook = serde_json::from_str(raw).context("invalid ipynb notebook")?;
        let mut metadata = NotebookMetadata::default();
        metadata.kernelspec = parsed
            .metadata
            .kernelspec
            .unwrap_or_else(Kernelspec::default);
        metadata.language_info = parsed
            .metadata
            .language_info
            .unwrap_or_else(LanguageInfo::default);
        metadata.extra = parsed.metadata.extra;
        if let Some(strata) = parsed.metadata.strata {
            if let Some(title) = strata.title {
                metadata.title = title;
            }
            metadata.description = strata.description;
        }

        let cells = parsed
            .cells
            .into_iter()
            .map(|cell| match cell {
                IpynbCell::Markdown {
                    id,
                    source,
                    metadata,
                    attachments,
                } => {
                    let mut cell_metadata = metadata;
                    if !attachments.is_empty() {
                        cell_metadata.insert("attachments".to_string(), Value::Object(attachments));
                    }
                    Ok(Cell {
                        id: CellId(id.unwrap_or_else(|| CellId::new().0)),
                        kind: CellKind::Markdown,
                        language: Language::Text,
                        source: source.join(),
                        execution_count: None,
                        outputs: Vec::new(),
                        metadata: cell_metadata,
                    })
                }
                IpynbCell::Raw {
                    id,
                    source,
                    metadata,
                    attachments,
                } => {
                    let mut cell_metadata = metadata;
                    if !attachments.is_empty() {
                        cell_metadata.insert("attachments".to_string(), Value::Object(attachments));
                    }
                    Ok(Cell {
                        id: CellId(id.unwrap_or_else(|| CellId::new().0)),
                        kind: CellKind::Raw,
                        language: Language::Text,
                        source: source.join(),
                        execution_count: None,
                        outputs: Vec::new(),
                        metadata: cell_metadata,
                    })
                }
                IpynbCell::Code {
                    id,
                    source,
                    metadata,
                    execution_count,
                    outputs,
                } => Ok(Cell {
                    id: CellId(id.unwrap_or_else(|| CellId::new().0)),
                    kind: CellKind::Code,
                    language: Language::Python,
                    source: source.join(),
                    execution_count,
                    outputs,
                    metadata,
                }),
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Notebook {
            metadata,
            nbformat: parsed.nbformat,
            nbformat_minor: parsed.nbformat_minor,
            cells,
        })
    }

    pub fn render_ipynb(notebook: &Notebook) -> String {
        let strata = json!({
            "title": notebook.metadata.title,
            "description": notebook.metadata.description,
        });
        let mut metadata_extra = notebook.metadata.extra.clone();
        metadata_extra.insert("strata".to_string(), strata);

        let metadata = IpynbMetadata {
            kernelspec: Some(notebook.metadata.kernelspec.clone()),
            language_info: Some(notebook.metadata.language_info.clone()),
            strata: None,
            extra: metadata_extra,
        };
        let cells = notebook
            .cells
            .iter()
            .map(|cell| match cell.kind {
                CellKind::Markdown => {
                    let (metadata, attachments) = split_attachments(&cell.metadata);
                    IpynbCell::Markdown {
                        id: Some(cell.id.0.clone()),
                        metadata,
                        source: split_lines(&cell.source).into(),
                        attachments,
                    }
                }
                CellKind::Raw => {
                    let (metadata, attachments) = split_attachments(&cell.metadata);
                    IpynbCell::Raw {
                        id: Some(cell.id.0.clone()),
                        metadata,
                        source: split_lines(&cell.source).into(),
                        attachments,
                    }
                }
                CellKind::Code | CellKind::Ai => IpynbCell::Code {
                    id: Some(cell.id.0.clone()),
                    metadata: cell.metadata.clone(),
                    execution_count: cell.execution_count,
                    source: split_lines(&cell.source).into(),
                    outputs: cell.outputs.clone(),
                },
            })
            .collect::<Vec<_>>();
        let rendered = IpynbNotebook {
            nbformat: notebook.nbformat,
            nbformat_minor: notebook.nbformat_minor,
            metadata,
            cells,
        };

        serde_json::to_string_pretty(&rendered).expect("ipynb render should succeed")
    }

    pub fn load_markdown(path: &Path) -> Result<Notebook> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read notebook at {}", path.display()))?;
        Self::parse_markdown(&raw)
    }

    pub fn save_markdown(path: &Path, notebook: &Notebook) -> Result<()> {
        let rendered = Self::render_markdown(notebook);
        fs::write(path, rendered)
            .with_context(|| format!("failed to write notebook at {}", path.display()))
    }

    pub fn parse_markdown(raw: &str) -> Result<Notebook> {
        let mut metadata = NotebookMetadata::default();
        let mut cells = Vec::new();
        let mut pending_meta: Option<(CellId, CellKind, Language)> = None;
        let mut text_buffer: Vec<String> = Vec::new();
        let lines: Vec<&str> = raw.lines().collect();
        let mut index = 0usize;

        while index < lines.len() {
            let line = lines[index];
            if index == 0 && line.starts_with("# ") {
                metadata.title = line.trim_start_matches("# ").trim().to_string();
                index += 1;
                continue;
            }

            if line.trim_start().starts_with("<!-- strata:cell") {
                if !text_buffer.is_empty() {
                    let source = join_and_trim(&text_buffer);
                    if !source.is_empty() {
                        cells.push(Cell::markdown(source));
                    }
                    text_buffer.clear();
                }
                pending_meta = Some(parse_cell_comment(line)?);
                index += 1;
                continue;
            }

            if line.trim_start().starts_with("```") {
                let fence = line.trim().trim_start_matches("```").trim();
                let language = parse_language(fence);
                let mut body = Vec::new();
                index += 1;
                while index < lines.len() && !lines[index].trim_start().starts_with("```") {
                    body.push(lines[index].to_string());
                    index += 1;
                }
                if index == lines.len() {
                    bail!("unclosed fenced block in notebook");
                }
                let source = body.join("\n");
                let cell = match pending_meta.take() {
                    Some((id, kind, meta_language)) => Cell {
                        id,
                        kind,
                        language: meta_language,
                        source,
                        execution_count: None,
                        outputs: Vec::new(),
                        metadata: BTreeMap::new(),
                    },
                    None => Cell::code(language, source),
                };
                cells.push(cell);
                index += 1;
                continue;
            }

            text_buffer.push(line.to_string());
            index += 1;
        }

        if !text_buffer.is_empty() {
            let source = join_and_trim(&text_buffer);
            if !source.is_empty() {
                cells.push(Cell::markdown(source));
            }
        }

        Ok(Notebook::new(metadata.title).with_cells(cells))
    }

    pub fn render_markdown(notebook: &Notebook) -> String {
        let mut output = String::new();
        output.push_str("# ");
        output.push_str(&notebook.metadata.title);
        output.push_str("\n\n");

        for (index, cell) in notebook.cells.iter().enumerate() {
            match cell.kind {
                CellKind::Markdown => {
                    output.push_str(cell.source.trim());
                    output.push('\n');
                }
                CellKind::Raw => {
                    output.push_str(&format!("<!-- strata:cell id={} kind=raw language=text -->\n", cell.id.0));
                    output.push_str("```text\n");
                    output.push_str(cell.source.trim_end());
                    output.push_str("\n```\n");
                }
                CellKind::Code | CellKind::Ai => {
                    output.push_str(&format!(
                        "<!-- strata:cell id={} kind={} language={} -->\n",
                        cell.id.0,
                        render_kind(cell.kind),
                        cell.language.fence_name()
                    ));
                    output.push_str("```");
                    output.push_str(cell.language.fence_name());
                    output.push('\n');
                    output.push_str(cell.source.trim_end());
                    output.push_str("\n```\n");
                }
            }

            if index + 1 < notebook.cells.len() {
                output.push('\n');
            }
        }

        output
    }
}

pub struct CheckpointStorage;

impl CheckpointStorage {
    pub fn exists(paths: &CheckpointPaths) -> bool {
        paths.manifest.exists()
    }

    pub fn save(paths: &CheckpointPaths, manifest: &SessionManifest) -> Result<()> {
        fs::create_dir_all(&paths.artifacts).with_context(|| {
            format!(
                "failed to create checkpoint dir {}",
                paths.artifacts.display()
            )
        })?;
        let body = serde_json::to_vec_pretty(manifest)?;
        fs::write(&paths.manifest, body)
            .with_context(|| format!("failed to write checkpoint {}", paths.manifest.display()))
    }

    pub fn load(paths: &CheckpointPaths) -> Result<SessionManifest> {
        let raw = fs::read_to_string(&paths.manifest)
            .with_context(|| format!("failed to read checkpoint {}", paths.manifest.display()))?;
        let manifest = serde_json::from_str(&raw)?;
        Ok(manifest)
    }
}

fn parse_cell_comment(line: &str) -> Result<(CellId, CellKind, Language)> {
    let inner = line
        .trim()
        .trim_start_matches("<!--")
        .trim_end_matches("-->")
        .trim();
    let payload = inner
        .strip_prefix("strata:cell")
        .map(str::trim)
        .context("invalid cell metadata comment")?;

    let mut id = None;
    let mut kind = None;
    let mut language = None;
    for token in payload.split_whitespace() {
        if let Some((key, value)) = token.split_once('=') {
            match key {
                "id" => id = Some(CellId(value.to_string())),
                "kind" => {
                    kind = Some(match value {
                        "code" => CellKind::Code,
                        "text" | "markdown" => CellKind::Markdown,
                        "raw" => CellKind::Raw,
                        "ai" => CellKind::Ai,
                        _ => bail!("unknown cell kind {value}"),
                    })
                }
                "language" => language = Some(parse_language(value)),
                _ => {}
            }
        }
    }

    Ok((
        id.context("cell metadata missing id")?,
        kind.context("cell metadata missing kind")?,
        language.context("cell metadata missing language")?,
    ))
}

fn parse_language(language: &str) -> Language {
    match language {
        "bash" | "sh" | "shell" => Language::Bash,
        "python" | "py" => Language::Python,
        "javascript" | "js" => Language::JavaScript,
        "typescript" | "ts" => Language::TypeScript,
        "ai" | "prompt" => Language::Ai,
        _ => Language::Text,
    }
}

fn render_kind(kind: CellKind) -> &'static str {
    match kind {
        CellKind::Code => "code",
        CellKind::Markdown => "markdown",
        CellKind::Raw => "raw",
        CellKind::Ai => "ai",
    }
}

fn join_and_trim(lines: &[String]) -> String {
    lines.join("\n").trim().to_string()
}

fn split_lines(source: &str) -> Vec<String> {
    if source.is_empty() {
        vec![String::new()]
    } else {
        source
            .split_inclusive('\n')
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    }
}

fn split_attachments(
    metadata: &BTreeMap<String, Value>,
) -> (BTreeMap<String, Value>, serde_json::Map<String, Value>) {
    let mut metadata = metadata.clone();
    let attachments = metadata
        .remove("attachments")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    (metadata, attachments)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct IpynbNotebook {
    nbformat: u8,
    nbformat_minor: u8,
    metadata: IpynbMetadata,
    cells: Vec<IpynbCell>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct IpynbMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kernelspec: Option<Kernelspec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language_info: Option<LanguageInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    strata: Option<StrataNotebookMetadata>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct StrataNotebookMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cell_type", rename_all = "snake_case")]
enum IpynbCell {
    Markdown {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
        #[serde(default)]
        source: SourceField,
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        attachments: serde_json::Map<String, Value>,
    },
    Raw {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
        #[serde(default)]
        source: SourceField,
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        attachments: serde_json::Map<String, Value>,
    },
    Code {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_count: Option<u32>,
        #[serde(default)]
        source: SourceField,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        outputs: Vec<CellOutput>,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(untagged)]
enum SourceField {
    String(String),
    Lines(Vec<String>),
    #[default]
    Empty,
}

impl SourceField {
    fn join(self) -> String {
        match self {
            SourceField::String(value) => value,
            SourceField::Lines(values) => values.concat(),
            SourceField::Empty => String::new(),
        }
    }
}

impl From<Vec<String>> for SourceField {
    fn from(value: Vec<String>) -> Self {
        SourceField::Lines(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_round_trip_preserves_code_metadata() {
        let notebook = Notebook::new("Demo").with_cells(vec![
            Cell::markdown("intro"),
            Cell::code(Language::Python, "value = 1\nprint(value)"),
            Cell::raw("plain"),
        ]);

        let rendered = NotebookStorage::render_markdown(&notebook);
        let parsed = NotebookStorage::parse_markdown(&rendered).unwrap();

        assert_eq!(parsed.metadata.title, "Demo");
        assert_eq!(parsed.cells.len(), 3);
        assert_eq!(parsed.cells[1].kind, CellKind::Code);
        assert_eq!(parsed.cells[1].language, Language::Python);
        assert_eq!(parsed.cells[1].source, "value = 1\nprint(value)");
        assert_eq!(parsed.cells[2].kind, CellKind::Raw);
    }

    #[test]
    fn ipynb_round_trip_preserves_outputs_and_metadata() {
        let mut notebook = Notebook::new("Demo").with_cells(vec![
            Cell::markdown("# Intro"),
            Cell::code(Language::Python, "print('hello')"),
        ]);
        notebook.cells[1].execution_count = Some(3);
        notebook.cells[1].outputs = vec![CellOutput::Stream {
            name: "stdout".to_string(),
            text: "hello\n".to_string(),
        }];
        notebook
            .cells[0]
            .metadata
            .insert("custom".to_string(), json!(true));

        let rendered = NotebookStorage::render_ipynb(&notebook);
        let parsed = NotebookStorage::parse_ipynb(&rendered).unwrap();

        assert_eq!(parsed.metadata.title, "Demo");
        assert_eq!(parsed.cells[1].execution_count, Some(3));
        assert_eq!(parsed.cells[1].primary_output_text(), "hello\n");
        assert_eq!(parsed.cells[0].metadata.get("custom"), Some(&json!(true)));
    }
}
