use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::core::{
    AiRunRecord, Cell, ContextBundle, ExecutionStatus, Language, Notebook, SessionManifest,
};

#[derive(Clone, Debug, Deserialize)]
pub struct ModelCatalog {
    #[serde(flatten)]
    pub providers: BTreeMap<String, ProviderCatalog>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderCatalog {
    pub id: String,
    pub api: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelDescriptor>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub name: Option<String>,
    pub modalities: Option<ModelModalities>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AiConfig {
    pub preferred_provider: Option<String>,
    pub preferred_model: Option<String>,
    pub models_url: String,
    pub openai_api_key: Option<String>,
    pub openai_base_url: String,
    pub anthropic_api_key: Option<String>,
    pub anthropic_base_url: String,
}

impl AiConfig {
    pub fn from_env() -> Self {
        Self {
            preferred_provider: std::env::var("STRATA_AI_PROVIDER").ok(),
            preferred_model: std::env::var("STRATA_AI_MODEL").ok(),
            models_url: std::env::var("STRATA_MODELS_DEV_URL")
                .unwrap_or_else(|_| "https://models.dev/api.json".to_string()),
            openai_api_key: env_var("OPENAI_API_KEY"),
            openai_base_url: std::env::var("STRATA_OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            anthropic_api_key: env_var("ANTHROPIC_API_KEY"),
            anthropic_base_url: std::env::var("STRATA_ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com/v1".to_string()),
        }
    }
}

pub trait AiProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn is_available(&self, config: &AiConfig) -> bool;
    fn run(
        &self,
        client: &Client,
        config: &AiConfig,
        model: &ModelDescriptor,
        prompt: &str,
        context: &ContextBundle,
    ) -> Result<String>;
}

pub trait ContextSelector: Send + Sync {
    fn select(
        &self,
        notebook: &Notebook,
        manifest: &SessionManifest,
        prompt_index: usize,
        max_items: usize,
    ) -> ContextBundle;
}

#[derive(Default)]
pub struct HeuristicContextSelector;

impl ContextSelector for HeuristicContextSelector {
    fn select(
        &self,
        notebook: &Notebook,
        manifest: &SessionManifest,
        prompt_index: usize,
        max_items: usize,
    ) -> ContextBundle {
        let start = prompt_index.saturating_sub(2);
        let end = usize::min(notebook.cells.len(), prompt_index + 3);
        let selected: Vec<&Cell> = notebook.cells[start..end]
            .iter()
            .filter(|cell| !cell.source.is_empty())
            .take(max_items)
            .collect();

        let mut snippets = selected
            .iter()
            .map(|cell| {
                let latest_output = manifest
                    .execution_history
                    .iter()
                    .rev()
                    .find(|record| record.cell_id == cell.id)
                    .map(|record| truncate(&record.output))
                    .unwrap_or_default();
                if latest_output.is_empty() {
                    format!(
                        "[{}:{}] {}",
                        label(cell.language),
                        cell.id.0,
                        truncate(&cell.source)
                    )
                } else {
                    format!(
                        "[{}:{}] {} | output: {}",
                        label(cell.language),
                        cell.id.0,
                        truncate(&cell.source),
                        latest_output
                    )
                }
            })
            .collect::<Vec<_>>();

        if !manifest.named_values.is_empty() {
            snippets.push(format!(
                "[named-values] {}",
                manifest
                    .named_values
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        let summary = format!(
            "Selected {} nearby cells and {} named values around prompt index {}",
            selected.len(),
            manifest.named_values.len(),
            prompt_index
        );
        let cell_ids = selected.iter().map(|cell| cell.id.0.clone()).collect();

        ContextBundle {
            summary,
            cell_ids,
            snippets,
        }
    }
}

#[derive(Clone)]
pub struct AiRuntime {
    client: Client,
    config: AiConfig,
    selector: Arc<dyn ContextSelector>,
    catalog: Option<ModelCatalog>,
    providers: Vec<Arc<dyn AiProvider>>,
}

impl AiRuntime {
    pub fn from_env() -> Result<Self> {
        Self::new(
            Client::builder().build()?,
            AiConfig::from_env(),
            Arc::new(HeuristicContextSelector),
        )
    }

    pub fn new(
        client: Client,
        config: AiConfig,
        selector: Arc<dyn ContextSelector>,
    ) -> Result<Self> {
        Ok(Self {
            client,
            config,
            selector,
            catalog: None,
            providers: vec![Arc::new(OpenAiProvider), Arc::new(AnthropicProvider)],
        })
    }

    pub fn execute(
        &mut self,
        notebook: &Notebook,
        manifest: &SessionManifest,
        prompt_index: usize,
    ) -> Result<AiRunRecord> {
        let provider = self.resolve_provider()?;
        let model = self.resolve_model(provider.id())?;
        let cell = notebook
            .cells
            .get(prompt_index)
            .context("ai prompt index out of bounds")?;
        let context = self.selector.select(notebook, manifest, prompt_index, 6);

        match provider.run(&self.client, &self.config, &model, &cell.source, &context) {
            Ok(response) => Ok(AiRunRecord {
                prompt_cell_id: cell.id.0.clone(),
                prompt: cell.source.clone(),
                context,
                provider_name: provider.id().to_string(),
                model_id: model.id.clone(),
                response,
                error_output: String::new(),
                status: ExecutionStatus::Succeeded,
            }),
            Err(error) => Ok(AiRunRecord {
                prompt_cell_id: cell.id.0.clone(),
                prompt: cell.source.clone(),
                context,
                provider_name: provider.id().to_string(),
                model_id: model.id.clone(),
                response: String::new(),
                error_output: error.to_string(),
                status: ExecutionStatus::Failed,
            }),
        }
    }

    fn resolve_provider(&self) -> Result<Arc<dyn AiProvider>> {
        if let Some(id) = &self.config.preferred_provider {
            let provider = self
                .providers
                .iter()
                .find(|provider| provider.id() == id)
                .context("preferred AI provider is not supported")?;
            if provider.is_available(&self.config) {
                return Ok(provider.clone());
            }
            bail!("preferred AI provider {} is not configured", id);
        }

        self.providers
            .iter()
            .find(|provider| provider.is_available(&self.config))
            .cloned()
            .context("no configured AI provider; set OPENAI_API_KEY or ANTHROPIC_API_KEY")
    }

    fn resolve_model(&mut self, provider_id: &str) -> Result<ModelDescriptor> {
        let preferred_model = self.config.preferred_model.clone();
        let catalog = self.load_catalog()?;
        let provider = catalog
            .providers
            .get(provider_id)
            .with_context(|| format!("provider {provider_id} not found in model catalog"))?;

        if let Some(requested) = preferred_model.as_ref() {
            return provider.models.get(requested).cloned().with_context(|| {
                format!("model {requested} not found for provider {provider_id}")
            });
        }

        provider
            .models
            .values()
            .filter(|model| supports_text_only(model))
            .min_by(|left, right| left.id.cmp(&right.id))
            .cloned()
            .context("no text-generation model found for provider")
    }

    fn load_catalog(&mut self) -> Result<&ModelCatalog> {
        if self.catalog.is_none() {
            let response = self
                .client
                .get(&self.config.models_url)
                .header("user-agent", "strata")
                .send()
                .context("failed to fetch models.dev catalog")?
                .error_for_status()
                .context("models.dev catalog request failed")?;
            self.catalog = Some(response.json().context("failed to decode model catalog")?);
        }
        Ok(self.catalog.as_ref().expect("catalog just initialized"))
    }
}

pub struct OpenAiProvider;

impl AiProvider for OpenAiProvider {
    fn id(&self) -> &'static str {
        "openai"
    }

    fn is_available(&self, config: &AiConfig) -> bool {
        config.openai_api_key.is_some()
    }

    fn run(
        &self,
        client: &Client,
        config: &AiConfig,
        model: &ModelDescriptor,
        prompt: &str,
        context: &ContextBundle,
    ) -> Result<String> {
        #[derive(Serialize)]
        struct Request<'a> {
            model: &'a str,
            instructions: String,
            input: &'a str,
        }

        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            output_text: String,
        }

        let response: Response = client
            .post(format!(
                "{}/responses",
                config.openai_base_url.trim_end_matches('/')
            ))
            .bearer_auth(
                config
                    .openai_api_key
                    .as_ref()
                    .context("OPENAI_API_KEY is not set")?,
            )
            .json(&Request {
                model: &model.id,
                instructions: render_instructions(context),
                input: prompt,
            })
            .send()
            .context("OpenAI request failed")?
            .error_for_status()
            .context("OpenAI returned an error status")?
            .json()
            .context("failed to decode OpenAI response")?;

        Ok(response.output_text)
    }
}

pub struct AnthropicProvider;

impl AiProvider for AnthropicProvider {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn is_available(&self, config: &AiConfig) -> bool {
        config.anthropic_api_key.is_some()
    }

    fn run(
        &self,
        client: &Client,
        config: &AiConfig,
        model: &ModelDescriptor,
        prompt: &str,
        context: &ContextBundle,
    ) -> Result<String> {
        #[derive(Serialize)]
        struct Message<'a> {
            role: &'a str,
            content: &'a str,
        }

        #[derive(Serialize)]
        struct Request<'a> {
            model: &'a str,
            max_tokens: u32,
            system: String,
            messages: Vec<Message<'a>>,
        }

        #[derive(Deserialize)]
        struct Content {
            #[serde(rename = "type")]
            kind: String,
            text: Option<String>,
        }

        #[derive(Deserialize)]
        struct Response {
            content: Vec<Content>,
        }

        let response: Response = client
            .post(format!(
                "{}/messages",
                config.anthropic_base_url.trim_end_matches('/')
            ))
            .header(
                "x-api-key",
                config
                    .anthropic_api_key
                    .as_ref()
                    .context("ANTHROPIC_API_KEY is not set")?,
            )
            .header("anthropic-version", "2023-06-01")
            .json(&Request {
                model: &model.id,
                max_tokens: 1024,
                system: render_instructions(context),
                messages: vec![Message {
                    role: "user",
                    content: prompt,
                }],
            })
            .send()
            .context("Anthropic request failed")?
            .error_for_status()
            .context("Anthropic returned an error status")?
            .json()
            .context("failed to decode Anthropic response")?;

        let text = response
            .content
            .into_iter()
            .find(|item| item.kind == "text")
            .and_then(|item| item.text)
            .context("Anthropic response did not include text content")?;
        Ok(text)
    }
}

fn supports_text_only(model: &ModelDescriptor) -> bool {
    let Some(modalities) = &model.modalities else {
        return true;
    };
    modalities.input.iter().any(|item| item == "text")
        && modalities.output.iter().any(|item| item == "text")
}

fn env_var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn render_instructions(context: &ContextBundle) -> String {
    format!(
        "You are operating inside a terminal-native notebook. Use the provided context when relevant.\nSummary: {}\nContext:\n{}",
        context.summary,
        context.snippets.join("\n")
    )
}

fn label(language: Language) -> &'static str {
    match language {
        Language::Bash => "bash",
        Language::Python => "python",
        Language::JavaScript => "javascript",
        Language::TypeScript => "typescript",
        Language::Text => "text",
        Language::Ai => "ai",
    }
}

fn truncate(source: &str) -> String {
    const LIMIT: usize = 72;
    if source.len() <= LIMIT {
        source.to_string()
    } else {
        format!("{}...", &source[..LIMIT])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Cell, Notebook};

    #[test]
    fn context_selector_prefers_nearby_cells_and_named_values() {
        let notebook = Notebook::new("AI").with_cells(vec![
            Cell::text("intro"),
            Cell::code(Language::Bash, "echo hi"),
            Cell::code(Language::Python, "value = 1"),
            Cell::ai("optimize this"),
        ]);
        let mut manifest = SessionManifest::new(&notebook);
        manifest
            .named_values
            .insert("shared".to_string(), "hello".to_string());

        let selector = HeuristicContextSelector;
        let bundle = selector.select(&notebook, &manifest, 3, 3);

        assert_eq!(bundle.cell_ids.len(), 3);
        assert!(
            bundle
                .snippets
                .iter()
                .any(|snippet| snippet.contains("value = 1"))
        );
        assert!(
            bundle
                .snippets
                .iter()
                .any(|snippet| snippet.contains("shared=hello"))
        );
    }
}
