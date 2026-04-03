use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::core::{Cell, CellId, CellKind, Language, Notebook, NotebookMetadata, SessionManifest};

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
                        cells.push(Cell::text(source));
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
                    },
                    None => Cell {
                        id: CellId::new(),
                        kind: if language == Language::Ai {
                            CellKind::Ai
                        } else {
                            CellKind::Code
                        },
                        language,
                        source,
                    },
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
                cells.push(Cell::text(source));
            }
        }

        Ok(Notebook { metadata, cells })
    }

    pub fn render_markdown(notebook: &Notebook) -> String {
        let mut output = String::new();
        output.push_str("# ");
        output.push_str(&notebook.metadata.title);
        output.push_str("\n\n");

        for (index, cell) in notebook.cells.iter().enumerate() {
            match cell.kind {
                CellKind::Text => {
                    output.push_str(cell.source.trim());
                    output.push('\n');
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
                        "text" => CellKind::Text,
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
        "ai" | "prompt" => Language::Ai,
        _ => Language::Text,
    }
}

fn render_kind(kind: CellKind) -> &'static str {
    match kind {
        CellKind::Code => "code",
        CellKind::Text => "text",
        CellKind::Ai => "ai",
    }
}

fn join_and_trim(lines: &[String]) -> String {
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_round_trip_preserves_code_metadata() {
        let notebook = Notebook::new("Demo").with_cells(vec![
            Cell::text("intro"),
            Cell::code(Language::Python, "value = 1\nprint(value)"),
            Cell::ai("optimize the function"),
        ]);

        let rendered = NotebookStorage::render_markdown(&notebook);
        let parsed = NotebookStorage::parse_markdown(&rendered).unwrap();

        assert_eq!(parsed.metadata.title, "Demo");
        assert_eq!(parsed.cells.len(), 3);
        assert_eq!(parsed.cells[1].kind, CellKind::Code);
        assert_eq!(parsed.cells[1].language, Language::Python);
        assert_eq!(parsed.cells[1].source, "value = 1\nprint(value)");
        assert_eq!(parsed.cells[2].kind, CellKind::Ai);
    }
}
