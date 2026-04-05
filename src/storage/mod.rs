use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::{
    Cell, CellId, CellKind, CellOutput, KernelKind, Kernelspec, Language, LanguageInfo, Notebook,
    NotebookMetadata, SessionManifest,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotebookFormat {
    Smd,
    Ipynb,
}

impl NotebookFormat {
    pub fn extension(self) -> &'static str {
        match self {
            NotebookFormat::Smd => "smd",
            NotebookFormat::Ipynb => "ipynb",
        }
    }
}

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
    pub fn format_for_path(path: &Path) -> NotebookFormat {
        match path.extension().and_then(|value| value.to_str()) {
            Some("ipynb") => NotebookFormat::Ipynb,
            _ => NotebookFormat::Smd,
        }
    }

    pub fn load(path: &Path) -> Result<Notebook> {
        match Self::format_for_path(path) {
            NotebookFormat::Ipynb => Self::load_ipynb(path),
            NotebookFormat::Smd => Self::load_smd(path),
        }
    }

    pub fn save(path: &Path, notebook: &Notebook) -> Result<()> {
        match Self::format_for_path(path) {
            NotebookFormat::Ipynb => Self::save_ipynb(path, notebook),
            NotebookFormat::Smd => Self::save_smd(path, notebook),
        }
    }

    pub fn render(path: Option<&Path>, notebook: &Notebook) -> String {
        match path
            .map(Self::format_for_path)
            .unwrap_or(NotebookFormat::Smd)
        {
            NotebookFormat::Ipynb => Self::render_ipynb(notebook),
            NotebookFormat::Smd => Self::render_smd(notebook),
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
            if let Some(kernel) = strata.kernel {
                metadata.runtime.kernel = kernel;
                metadata.kernelspec = kernel.kernelspec();
                metadata.language_info = kernel.language_info();
            }
            if let Some(environment) = strata.environment {
                metadata.runtime.environment = environment;
            }
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
            "kernel": notebook.metadata.runtime.kernel,
            "environment": notebook.metadata.runtime.environment,
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

    pub fn load_smd(path: &Path) -> Result<Notebook> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read notebook at {}", path.display()))?;
        Self::parse_smd(&raw)
    }

    pub fn save_smd(path: &Path, notebook: &Notebook) -> Result<()> {
        let mut notebook = notebook.clone();
        materialize_image_outputs(path, &mut notebook)?;
        let rendered = Self::render_smd(&notebook);
        fs::write(path, rendered)
            .with_context(|| format!("failed to write notebook at {}", path.display()))
    }

    pub fn parse_smd(raw: &str) -> Result<Notebook> {
        let mut metadata = NotebookMetadata::default();
        let mut cells = Vec::new();
        let lines: Vec<&str> = raw.lines().collect();
        let mut index = 0usize;

        while index < lines.len() {
            let line = lines[index];
            let trimmed = line.trim();

            if trimmed.starts_with("<!-- strata:format") {
                index += 1;
                continue;
            }

            if trimmed.starts_with("<!-- strata:notebook") {
                parse_notebook_comment(trimmed, &mut metadata)?;
                index += 1;
                continue;
            }

            if trimmed.starts_with("<!-- strata:cell") {
                let (id, kind, language, execution_count) = parse_cell_comment(trimmed)?;
                index += 1;

                while index < lines.len() && lines[index].trim().is_empty() {
                    index += 1;
                }

                if index >= lines.len() || !lines[index].trim_start().starts_with("```") {
                    bail!("cell missing fenced content block");
                }
                let fence = lines[index].trim().trim_start_matches("```").trim();
                let mut body = Vec::new();
                index += 1;
                while index < lines.len() && !lines[index].trim_start().starts_with("```") {
                    body.push(lines[index].to_string());
                    index += 1;
                }
                if index == lines.len() {
                    bail!("unclosed fenced block in notebook");
                }
                let mut cell = Cell {
                    id,
                    kind,
                    language: if kind == CellKind::Code {
                        parse_language(fence)
                    } else {
                        language
                    },
                    source: body.join("\n"),
                    execution_count,
                    outputs: Vec::new(),
                    metadata: BTreeMap::new(),
                };
                index += 1;

                loop {
                    while index < lines.len() && lines[index].trim().is_empty() {
                        index += 1;
                    }
                    if index >= lines.len()
                        || !lines[index].trim_start().starts_with("<!-- strata:output")
                    {
                        break;
                    }
                    let output_meta = parse_output_comment(lines[index].trim())?;
                    index += 1;
                    while index < lines.len() && lines[index].trim().is_empty() {
                        index += 1;
                    }
                    if index >= lines.len() || !lines[index].trim_start().starts_with("```") {
                        bail!("output missing fenced content block");
                    }
                    let mut output_body = Vec::new();
                    index += 1;
                    while index < lines.len() && !lines[index].trim_start().starts_with("```") {
                        output_body.push(lines[index].to_string());
                        index += 1;
                    }
                    if index == lines.len() {
                        bail!("unclosed output fenced block in notebook");
                    }
                    index += 1;
                    cell.outputs
                        .push(output_from_meta(output_meta, output_body.join("\n")));
                }
                cells.push(cell);
                continue;
            }
            index += 1;
        }

        Ok(Notebook {
            metadata,
            nbformat: 4,
            nbformat_minor: 5,
            cells,
        })
    }

    pub fn render_smd(notebook: &Notebook) -> String {
        let mut output = String::new();
        output.push_str("<!-- strata:format version=1 -->\n");
        output.push_str(&format!(
            "<!-- strata:notebook title={:?}{} kernel={:?} environment={:?} -->\n\n",
            notebook.metadata.title,
            notebook
                .metadata
                .description
                .as_ref()
                .map(|value| format!(" description={value:?}"))
                .unwrap_or_default(),
            render_kernel(notebook.metadata.runtime.kernel),
            notebook.metadata.runtime.environment
        ));

        for (index, cell) in notebook.cells.iter().enumerate() {
            output.push_str(&format!(
                "<!-- strata:cell id={} kind={} language={}{} -->\n",
                cell.id.0,
                render_kind(cell.kind),
                cell.language.fence_name(),
                cell.execution_count
                    .map(|value| format!(" execution_count={value}"))
                    .unwrap_or_default()
            ));
            output.push_str("```");
            output.push_str(match cell.kind {
                CellKind::Markdown => "markdown",
                CellKind::Raw => "raw",
                _ => cell.language.fence_name(),
            });
            output.push('\n');
            output.push_str(cell.source.trim_end());
            output.push_str("\n```\n");

            for output_block in &cell.outputs {
                output.push('\n');
                output.push_str(&render_output_comment(output_block));
                output.push('\n');
                output.push_str("```text\n");
                output.push_str(&output_block.as_text());
                output.push_str("\n```\n");
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

fn parse_notebook_comment(line: &str, metadata: &mut NotebookMetadata) -> Result<()> {
    let inner = line
        .trim()
        .trim_start_matches("<!--")
        .trim_end_matches("-->")
        .trim();
    let payload = inner
        .strip_prefix("strata:notebook")
        .map(str::trim)
        .context("invalid notebook metadata comment")?;

    for token in split_metadata_tokens(payload) {
        if let Some((key, value)) = token.split_once('=') {
            let value = unquote(value);
            match key {
                "title" => metadata.title = value.to_string(),
                "description" => metadata.description = Some(value.to_string()),
                "kernel" => {
                    let kernel = parse_kernel(value)?;
                    metadata.runtime.kernel = kernel;
                    metadata.kernelspec = kernel.kernelspec();
                    metadata.language_info = kernel.language_info();
                }
                "environment" => metadata.runtime.environment = value.to_string(),
                _ => {}
            }
        }
    }

    Ok(())
}

fn parse_cell_comment(line: &str) -> Result<(CellId, CellKind, Language, Option<u32>)> {
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
    let mut execution_count = None;
    for token in split_metadata_tokens(payload) {
        if let Some((key, value)) = token.split_once('=') {
            match key {
                "id" => id = Some(CellId(unquote(value).to_string())),
                "kind" => {
                    kind = Some(match value {
                        "code" => CellKind::Code,
                        "text" | "markdown" => CellKind::Markdown,
                        "raw" => CellKind::Raw,
                        "ai" => CellKind::Ai,
                        _ => bail!("unknown cell kind {value}"),
                    })
                }
                "language" => language = Some(parse_language(unquote(value))),
                "execution_count" => execution_count = Some(unquote(value).parse()?),
                _ => {}
            }
        }
    }

    Ok((
        id.context("cell metadata missing id")?,
        kind.context("cell metadata missing kind")?,
        language.context("cell metadata missing language")?,
        execution_count,
    ))
}

#[derive(Clone, Debug)]
struct OutputMeta {
    kind: String,
    name: Option<String>,
    execution_count: Option<u32>,
    ename: Option<String>,
    evalue: Option<String>,
    mime: Option<String>,
    path: Option<String>,
}

fn parse_output_comment(line: &str) -> Result<OutputMeta> {
    let inner = line
        .trim()
        .trim_start_matches("<!--")
        .trim_end_matches("-->")
        .trim();
    let payload = inner
        .strip_prefix("strata:output")
        .map(str::trim)
        .context("invalid output metadata comment")?;

    let mut meta = OutputMeta {
        kind: "stream".to_string(),
        name: None,
        execution_count: None,
        ename: None,
        evalue: None,
        mime: None,
        path: None,
    };
    for token in split_metadata_tokens(payload) {
        if let Some((key, value)) = token.split_once('=') {
            let value = unquote(value).to_string();
            match key {
                "kind" => meta.kind = value,
                "name" => meta.name = Some(value),
                "execution_count" => meta.execution_count = Some(value.parse()?),
                "ename" => meta.ename = Some(value),
                "evalue" => meta.evalue = Some(value),
                "mime" => meta.mime = Some(value),
                "path" => meta.path = Some(value),
                _ => {}
            }
        }
    }
    Ok(meta)
}

fn output_from_meta(meta: OutputMeta, text: String) -> CellOutput {
    match meta.kind.as_str() {
        "execute_result" => CellOutput::ExecuteResult {
            execution_count: meta.execution_count.unwrap_or(0),
            data: BTreeMap::from([("text/plain".to_string(), Value::String(text))]),
            metadata: image_output_metadata(&meta),
        },
        "display_data" => CellOutput::DisplayData {
            data: BTreeMap::from([("text/plain".to_string(), Value::String(text))]),
            metadata: image_output_metadata(&meta),
        },
        "error" => CellOutput::Error {
            ename: meta.ename.unwrap_or_else(|| "Error".to_string()),
            evalue: meta.evalue.unwrap_or_default(),
            traceback: text.lines().map(ToString::to_string).collect(),
        },
        _ => CellOutput::Stream {
            name: meta.name.unwrap_or_else(|| "stdout".to_string()),
            text,
        },
    }
}

fn render_output_comment(output: &CellOutput) -> String {
    match output {
        CellOutput::Stream { name, .. } => {
            format!("<!-- strata:output kind=stream name={name:?} -->")
        }
        CellOutput::ExecuteResult {
            execution_count,
            metadata,
            ..
        } => format!(
            "<!-- strata:output kind=execute_result execution_count={}{}{} -->",
            execution_count,
            metadata
                .get("strata_image_mime")
                .and_then(Value::as_str)
                .map(|mime| format!(" mime={mime:?}"))
                .unwrap_or_default(),
            metadata
                .get("strata_image_path")
                .and_then(Value::as_str)
                .map(|path| format!(" path={path:?}"))
                .unwrap_or_default()
        ),
        CellOutput::DisplayData { metadata, .. } => format!(
            "<!-- strata:output kind=display_data{}{} -->",
            metadata
                .get("strata_image_mime")
                .and_then(Value::as_str)
                .map(|mime| format!(" mime={mime:?}"))
                .unwrap_or_default(),
            metadata
                .get("strata_image_path")
                .and_then(Value::as_str)
                .map(|path| format!(" path={path:?}"))
                .unwrap_or_default()
        ),
        CellOutput::Error { ename, evalue, .. } => {
            format!("<!-- strata:output kind=error ename={ename:?} evalue={evalue:?} -->")
        }
    }
}

fn image_output_metadata(meta: &OutputMeta) -> BTreeMap<String, Value> {
    let mut metadata = BTreeMap::new();
    if let Some(mime) = &meta.mime {
        metadata.insert("strata_image_mime".to_string(), Value::String(mime.clone()));
    }
    if let Some(path) = &meta.path {
        metadata.insert("strata_image_path".to_string(), Value::String(path.clone()));
    }
    metadata
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

fn parse_kernel(value: &str) -> Result<KernelKind> {
    Ok(match value {
        "python" => KernelKind::Python,
        "bash" => KernelKind::Bash,
        "javascript" | "js" => KernelKind::JavaScript,
        other => bail!("unknown kernel {other}"),
    })
}

fn render_kernel(kernel: KernelKind) -> &'static str {
    match kernel {
        KernelKind::Python => "python",
        KernelKind::Bash => "bash",
        KernelKind::JavaScript => "javascript",
    }
}

fn split_metadata_tokens(payload: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut quote_char = '\0';

    for ch in payload.chars() {
        if in_quotes {
            current.push(ch);
            if ch == quote_char {
                in_quotes = false;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            in_quotes = true;
            quote_char = ch;
            current.push(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(current.clone());
                current.clear();
            }
            continue;
        }
        current.push(ch);
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value)
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

fn materialize_image_outputs(path: &Path, notebook: &mut Notebook) -> Result<()> {
    let artifacts_dir = CheckpointPaths::for_notebook(path).artifacts;
    fs::create_dir_all(&artifacts_dir)?;
    let notebook_dir = path.parent().unwrap_or_else(|| Path::new("."));

    for cell in &mut notebook.cells {
        for (index, output) in cell.outputs.iter_mut().enumerate() {
            let Some(image) = output.image_info() else {
                continue;
            };
            if image.path.is_some() {
                continue;
            }
            let extension = match image.mime.as_str() {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/svg+xml" => "svg",
                "image/gif" => "gif",
                _ => continue,
            };
            let artifact_path = artifacts_dir.join(format!("{}-{index}.{extension}", cell.id.0));
            let Some(data) = image.data else {
                continue;
            };
            match image.mime.as_str() {
                "image/svg+xml" => {
                    fs::write(&artifact_path, data.as_str().unwrap_or_default().as_bytes())?;
                }
                _ => {
                    let encoded = data.as_str().unwrap_or_default();
                    let decoded = base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .context("failed to decode image payload")?;
                    fs::write(&artifact_path, decoded)?;
                }
            }

            let relative = artifact_path
                .strip_prefix(notebook_dir)
                .unwrap_or(&artifact_path)
                .display()
                .to_string();
            match output {
                CellOutput::ExecuteResult { metadata, .. }
                | CellOutput::DisplayData { metadata, .. } => {
                    metadata.insert("strata_image_path".to_string(), Value::String(relative));
                    metadata.insert("strata_image_mime".to_string(), Value::String(image.mime));
                }
                _ => {}
            }
        }
    }
    Ok(())
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kernel: Option<KernelKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cell_type", rename_all = "snake_case")]
enum IpynbCell {
    Markdown {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default)]
        metadata: BTreeMap<String, Value>,
        #[serde(default)]
        source: SourceField,
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        attachments: serde_json::Map<String, Value>,
    },
    Raw {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default)]
        metadata: BTreeMap<String, Value>,
        #[serde(default)]
        source: SourceField,
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        attachments: serde_json::Map<String, Value>,
    },
    Code {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default)]
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
    fn smd_round_trip_preserves_code_metadata_and_outputs() {
        let notebook = Notebook::new("Demo").with_cells(vec![
            Cell::markdown("intro"),
            Cell::code(Language::Python, "value = 1\nprint(value)"),
            Cell::raw("plain"),
        ]);
        let mut notebook = notebook;
        notebook.metadata.runtime.kernel = KernelKind::Bash;
        notebook.metadata.runtime.environment = "none".to_string();
        notebook.cells[1].execution_count = Some(2);
        notebook.cells[1].outputs = vec![CellOutput::Stream {
            name: "stdout".to_string(),
            text: "1\n".to_string(),
        }];

        let rendered = NotebookStorage::render_smd(&notebook);
        let parsed = NotebookStorage::parse_smd(&rendered).unwrap();

        assert_eq!(parsed.metadata.title, "Demo");
        assert_eq!(parsed.cells.len(), 3);
        assert_eq!(parsed.cells[1].kind, CellKind::Code);
        assert_eq!(parsed.cells[1].language, Language::Python);
        assert_eq!(parsed.cells[1].source, "value = 1\nprint(value)");
        assert_eq!(parsed.cells[1].execution_count, Some(2));
        assert_eq!(parsed.cells[1].primary_output_text(), "1\n");
        assert_eq!(parsed.cells[2].kind, CellKind::Raw);
        assert_eq!(parsed.metadata.runtime.kernel, KernelKind::Bash);
        assert_eq!(parsed.metadata.runtime.environment, "none");
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
        notebook.cells[0]
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
