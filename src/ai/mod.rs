use crate::core::{Cell, CellKind, Language};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextBundle {
    pub summary: String,
    pub cell_ids: Vec<String>,
    pub snippets: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiRunRecord {
    pub prompt_cell_id: String,
    pub context: ContextBundle,
    pub provider_name: String,
    pub response: String,
}

pub trait AiProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, prompt: &str, context: &ContextBundle) -> anyhow::Result<String>;
}

pub trait ContextSelector: Send + Sync {
    fn select(
        &self,
        notebook_cells: &[Cell],
        prompt_index: usize,
        max_items: usize,
    ) -> ContextBundle;
}

#[derive(Default)]
pub struct HeuristicContextSelector;

impl ContextSelector for HeuristicContextSelector {
    fn select(
        &self,
        notebook_cells: &[Cell],
        prompt_index: usize,
        max_items: usize,
    ) -> ContextBundle {
        let start = prompt_index.saturating_sub(2);
        let end = usize::min(notebook_cells.len(), prompt_index + 3);
        let selected: Vec<&Cell> = notebook_cells[start..end]
            .iter()
            .filter(|cell| cell.kind != CellKind::Text || !cell.source.is_empty())
            .take(max_items)
            .collect();

        let summary = format!(
            "Selected {} nearby cells around prompt index {}",
            selected.len(),
            prompt_index
        );
        let cell_ids = selected.iter().map(|cell| cell.id.0.clone()).collect();
        let snippets = selected
            .iter()
            .map(|cell| format!("[{}] {}", label(cell.language), truncate(&cell.source)))
            .collect();

        ContextBundle {
            summary,
            cell_ids,
            snippets,
        }
    }
}

pub struct EchoProvider;

impl AiProvider for EchoProvider {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn run(&self, prompt: &str, context: &ContextBundle) -> anyhow::Result<String> {
        Ok(format!(
            "Prompt: {prompt}\nContext: {}",
            context.snippets.join(" | ")
        ))
    }
}

fn label(language: Language) -> &'static str {
    match language {
        Language::Bash => "bash",
        Language::Python => "python",
        Language::Text => "text",
        Language::Ai => "ai",
    }
}

fn truncate(source: &str) -> String {
    const LIMIT: usize = 48;
    if source.len() <= LIMIT {
        source.to_string()
    } else {
        format!("{}...", &source[..LIMIT])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Cell, Language};

    #[test]
    fn context_selector_prefers_nearby_cells() {
        let cells = vec![
            Cell::text("intro"),
            Cell::code(Language::Bash, "echo hi"),
            Cell::code(Language::Python, "value = 1"),
            Cell::ai("optimize this"),
        ];

        let selector = HeuristicContextSelector;
        let bundle = selector.select(&cells, 3, 3);

        assert_eq!(bundle.cell_ids.len(), 3);
        assert!(
            bundle
                .snippets
                .iter()
                .any(|snippet| snippet.contains("value = 1"))
        );
    }
}
