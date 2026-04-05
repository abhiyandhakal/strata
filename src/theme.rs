use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginCapability {
    Theme,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginSource {
    BuiltIn,
    UserDir(PathBuf),
    NotebookDir(PathBuf),
    ExplicitPath(PathBuf),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub capabilities: Vec<ManifestCapability>,
    pub theme: Option<ThemePluginRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ManifestCapability {
    Theme,
}

impl ManifestCapability {
    fn capability(&self) -> PluginCapability {
        match self {
            Self::Theme => PluginCapability::Theme,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ThemePluginRef {
    pub spec: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ThemeSpec {
    pub name: Option<String>,
    #[serde(default)]
    pub styles: BTreeMap<String, ThemeRecipe>,
    #[serde(default)]
    pub syntax: SyntaxThemeSpec,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct SyntaxThemeSpec {
    pub comment: Option<ThemeRecipe>,
    pub string: Option<ThemeRecipe>,
    pub number: Option<ThemeRecipe>,
    pub type_name: Option<ThemeRecipe>,
    pub keyword: Option<ThemeRecipe>,
    pub identifier: Option<ThemeRecipe>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ThemeRecipe {
    pub fg: Option<String>,
    pub bg: Option<String>,
    #[serde(default)]
    pub modifiers: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SyntaxTokenKind {
    Comment,
    String,
    Number,
    TypeName,
    Keyword,
    Identifier,
}

#[derive(Clone, Debug)]
pub struct Theme {
    name: String,
    source: PluginSource,
    styles: BTreeMap<String, Style>,
    syntax: BTreeMap<SyntaxTokenKind, Style>,
}

#[derive(Clone, Debug)]
pub struct ThemeResolution {
    pub theme: Theme,
    pub warning: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ThemeResolver {
    user_plugin_root: Option<PathBuf>,
}

impl ThemeResolver {
    pub fn from_env() -> Self {
        Self::new(
            std::env::var("XDG_CONFIG_HOME").ok(),
            std::env::var("HOME").ok(),
        )
    }

    pub fn new(xdg_config_home: Option<String>, home: Option<String>) -> Self {
        let user_plugin_root = if let Some(xdg) = xdg_config_home {
            Some(PathBuf::from(xdg).join("strata").join("plugins"))
        } else {
            home.map(|home| {
                PathBuf::from(home)
                    .join(".config")
                    .join("strata")
                    .join("plugins")
            })
        };
        Self { user_plugin_root }
    }

    pub fn resolve(&self, configured_path: Option<&str>, notebook_path: Option<&Path>) -> ThemeResolution {
        match configured_path {
            None => ThemeResolution {
                theme: Theme::default_theme(),
                warning: None,
            },
            Some(path) => match self.load_theme(path, notebook_path) {
                Ok(theme) => ThemeResolution {
                    theme,
                    warning: None,
                },
                Err(err) => ThemeResolution {
                    theme: Theme::default_theme(),
                    warning: Some(format!("theme load failed: {err}")),
                },
            },
        }
    }

    fn load_theme(&self, configured_path: &str, notebook_path: Option<&Path>) -> Result<Theme> {
        let plugin = self.load_plugin(configured_path, notebook_path)?;
        if !plugin
            .manifest
            .capabilities
            .iter()
            .any(|capability| capability.capability() == PluginCapability::Theme)
        {
            bail!("plugin {} does not declare theme capability", plugin.manifest.id);
        }
        let theme_ref = plugin
            .manifest
            .theme
            .as_ref()
            .context("theme plugin is missing [theme] spec entry")?;
        let spec_path = plugin.root.join(&theme_ref.spec);
        let body = fs::read_to_string(&spec_path)
            .with_context(|| format!("failed to read theme spec {}", spec_path.display()))?;
        let spec = toml::from_str::<ThemeSpec>(&body)
            .with_context(|| format!("failed to parse theme spec {}", spec_path.display()))?;
        Theme::compile(
            spec.name.clone().unwrap_or_else(|| plugin.manifest.name.clone()),
            plugin.source,
            &spec,
        )
    }

    fn load_plugin(&self, configured_path: &str, notebook_path: Option<&Path>) -> Result<DiscoveredPlugin> {
        let candidate_roots = self.candidate_paths(configured_path, notebook_path);
        for candidate in candidate_roots {
            if let Some(plugin) = load_plugin_candidate(&candidate)? {
                return Ok(plugin);
            }
        }
        bail!("theme plugin `{configured_path}` was not found");
    }

    fn candidate_paths(&self, configured_path: &str, notebook_path: Option<&Path>) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let configured = PathBuf::from(configured_path);
        if configured.is_absolute() {
            paths.push(configured);
            return paths;
        }

        if let Some(notebook_dir) = notebook_path.and_then(Path::parent) {
            paths.push(notebook_dir.join(&configured));
            paths.push(notebook_dir.join(".strata").join("plugins").join(&configured));
        }
        if let Some(user_root) = &self.user_plugin_root {
            paths.push(user_root.join(&configured));
        }
        paths
    }
}

#[derive(Clone, Debug)]
struct DiscoveredPlugin {
    root: PathBuf,
    source: PluginSource,
    manifest: PluginManifest,
}

fn load_plugin_candidate(candidate: &Path) -> Result<Option<DiscoveredPlugin>> {
    let (manifest_path, root, source) = if candidate.is_file() {
        let root = candidate
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        (
            candidate.to_path_buf(),
            root,
            PluginSource::ExplicitPath(candidate.to_path_buf()),
        )
    } else {
        let manifest_path = candidate.join("plugin.toml");
        if !manifest_path.exists() {
            return Ok(None);
        }
        let source = if candidate.components().any(|part| part.as_os_str() == ".strata") {
            PluginSource::NotebookDir(candidate.to_path_buf())
        } else {
            PluginSource::UserDir(candidate.to_path_buf())
        };
        (manifest_path, candidate.to_path_buf(), source)
    };

    if !manifest_path.exists() {
        return Ok(None);
    }
    let body = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read plugin manifest {}", manifest_path.display()))?;
    let manifest = toml::from_str::<PluginManifest>(&body)
        .with_context(|| format!("failed to parse plugin manifest {}", manifest_path.display()))?;
    Ok(Some(DiscoveredPlugin {
        root,
        source,
        manifest,
    }))
}

impl Theme {
    pub fn default_theme() -> Self {
        let spec = ThemeSpec {
            name: Some("Strata Default".to_string()),
            styles: default_style_recipes(),
            syntax: SyntaxThemeSpec {
                comment: Some(recipe("darkgray", None, &[])),
                string: Some(recipe("green", None, &[])),
                number: Some(recipe("cyan", None, &[])),
                type_name: Some(recipe("yellow", None, &[])),
                keyword: Some(recipe("magenta", None, &["bold"])),
                identifier: Some(recipe("lightblue", None, &[])),
            },
        };
        Self::compile("Strata Default".to_string(), PluginSource::BuiltIn, &spec)
            .expect("default theme must compile")
    }

    fn compile(name: String, source: PluginSource, spec: &ThemeSpec) -> Result<Self> {
        let mut styles = BTreeMap::new();
        for (key, recipe) in default_style_recipes() {
            styles.insert(key, compile_recipe(&recipe)?);
        }
        for (key, recipe) in &spec.styles {
            styles.insert(key.clone(), compile_recipe(recipe)?);
        }

        let mut syntax = BTreeMap::from([
            (
                SyntaxTokenKind::Comment,
                compile_recipe(
                    spec.syntax
                        .comment
                        .as_ref()
                        .unwrap_or(&recipe("darkgray", None, &[])),
                )?,
            ),
            (
                SyntaxTokenKind::String,
                compile_recipe(
                    spec.syntax
                        .string
                        .as_ref()
                        .unwrap_or(&recipe("green", None, &[])),
                )?,
            ),
            (
                SyntaxTokenKind::Number,
                compile_recipe(
                    spec.syntax
                        .number
                        .as_ref()
                        .unwrap_or(&recipe("cyan", None, &[])),
                )?,
            ),
            (
                SyntaxTokenKind::TypeName,
                compile_recipe(
                    spec.syntax
                        .type_name
                        .as_ref()
                        .unwrap_or(&recipe("yellow", None, &[])),
                )?,
            ),
            (
                SyntaxTokenKind::Keyword,
                compile_recipe(
                    spec.syntax
                        .keyword
                        .as_ref()
                        .unwrap_or(&recipe("magenta", None, &["bold"])),
                )?,
            ),
            (
                SyntaxTokenKind::Identifier,
                compile_recipe(
                    spec.syntax
                        .identifier
                        .as_ref()
                        .unwrap_or(&recipe("lightblue", None, &[])),
                )?,
            ),
        ]);

        if let Some(base) = styles.get("text.default").copied() {
            syntax.entry(SyntaxTokenKind::Identifier).or_insert(base);
        }

        Ok(Self {
            name,
            source,
            styles,
            syntax,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn source(&self) -> &PluginSource {
        &self.source
    }

    pub fn style(&self, key: &str) -> Style {
        self.styles
            .get(key)
            .copied()
            .or_else(|| self.styles.get("text.default").copied())
            .unwrap_or_else(Style::default)
    }

    pub fn syntax_style(&self, token: SyntaxTokenKind) -> Style {
        self.syntax
            .get(&token)
            .copied()
            .unwrap_or_else(|| self.style("text.default"))
    }
}

fn default_style_recipes() -> BTreeMap<String, ThemeRecipe> {
    BTreeMap::from([
        ("text.default".to_string(), recipe("white", None, &[])),
        ("status.title".to_string(), recipe("white", None, &["bold"])),
        ("status.body".to_string(), recipe("gray", None, &[])),
        ("toolbar.block".to_string(), recipe("white", Some("#0b1020"), &[])),
        ("toolbar.border".to_string(), recipe("lightcyan", None, &["bold"])),
        ("toolbar.button.save".to_string(), recipe("yellow", None, &["bold"])),
        ("toolbar.button.run_all".to_string(), recipe("green", None, &["bold"])),
        ("toolbar.button.restart".to_string(), recipe("red", None, &["bold"])),
        ("toolbar.button.add_code".to_string(), recipe("cyan", None, &["bold"])),
        ("toolbar.button.add_markdown".to_string(), recipe("blue", None, &["bold"])),
        ("notebook.empty".to_string(), recipe("gray", None, &[])),
        ("cell.shell".to_string(), recipe("white", Some("#0a0e14"), &[])),
        ("cell.shell.selected".to_string(), recipe("white", Some("#14222e"), &[])),
        ("cell.border".to_string(), recipe("darkgray", None, &[])),
        ("cell.border.selected".to_string(), recipe("lightcyan", None, &["bold"])),
        ("cell.prompt".to_string(), recipe("gray", None, &[])),
        ("cell.prompt.selected".to_string(), recipe("black", Some("lightcyan"), &["bold"])),
        ("cell.button.run".to_string(), recipe("green", None, &["bold"])),
        ("cell.button.edit".to_string(), recipe("yellow", None, &["bold"])),
        ("cell.button.add".to_string(), recipe("cyan", None, &["bold"])),
        ("cell.button.delete".to_string(), recipe("red", None, &["bold"])),
        ("cell.button.output".to_string(), recipe("blue", None, &["bold"])),
        ("cell.title".to_string(), recipe("white", None, &["bold"])),
        (
            "editor.cursor_line".to_string(),
            ThemeRecipe {
                fg: None,
                bg: Some("#1b2230".to_string()),
                modifiers: Vec::new(),
            },
        ),
        ("editor.cursor.normal".to_string(), recipe("black", Some("lightcyan"), &[])),
        ("editor.cursor.insert".to_string(), recipe("black", Some("lightblue"), &[])),
        (
            "editor.cursor.visual".to_string(),
            recipe("black", Some("lightyellow"), &[]),
        ),
        (
            "editor.cursor.operator".to_string(),
            recipe("black", Some("lightgreen"), &[]),
        ),
        ("markdown.heading1".to_string(), recipe("yellow", None, &["bold"])),
        ("markdown.heading2".to_string(), recipe("cyan", None, &["bold"])),
        ("output.block".to_string(), recipe("white", Some("#0e1520"), &[])),
        ("output.border".to_string(), recipe("darkgray", None, &[])),
        ("output.stream.label".to_string(), recipe("blue", None, &["bold"])),
        ("output.result.label".to_string(), recipe("green", None, &["bold"])),
        ("output.error.label".to_string(), recipe("red", None, &["bold"])),
        ("output.error.trace".to_string(), recipe("lightred", None, &[])),
        ("lsp.active".to_string(), recipe("green", None, &["bold"])),
        ("lsp.available".to_string(), recipe("cyan", None, &["bold"])),
        ("lsp.unavailable".to_string(), recipe("yellow", None, &[])),
    ])
}

fn recipe(fg: &str, bg: Option<&str>, modifiers: &[&str]) -> ThemeRecipe {
    ThemeRecipe {
        fg: Some(fg.to_string()),
        bg: bg.map(str::to_string),
        modifiers: modifiers.iter().map(|value| value.to_string()).collect(),
    }
}

fn compile_recipe(recipe: &ThemeRecipe) -> Result<Style> {
    let mut style = Style::default();
    if let Some(fg) = &recipe.fg {
        style = style.fg(parse_color(fg)?);
    }
    if let Some(bg) = &recipe.bg {
        style = style.bg(parse_color(bg)?);
    }
    for modifier in &recipe.modifiers {
        style = style.add_modifier(parse_modifier(modifier)?);
    }
    Ok(style)
}

fn parse_modifier(value: &str) -> Result<Modifier> {
    match value.trim().to_ascii_lowercase().as_str() {
        "bold" => Ok(Modifier::BOLD),
        "dim" => Ok(Modifier::DIM),
        "italic" => Ok(Modifier::ITALIC),
        "underlined" | "underline" => Ok(Modifier::UNDERLINED),
        "slow_blink" | "blink" => Ok(Modifier::SLOW_BLINK),
        "rapid_blink" => Ok(Modifier::RAPID_BLINK),
        "reversed" | "reverse" => Ok(Modifier::REVERSED),
        "hidden" => Ok(Modifier::HIDDEN),
        "crossed_out" | "crossed" => Ok(Modifier::CROSSED_OUT),
        other => bail!("unknown modifier `{other}`"),
    }
}

fn parse_color(value: &str) -> Result<Color> {
    let value = value.trim().to_ascii_lowercase();
    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() == 6 {
            let red = u8::from_str_radix(&hex[0..2], 16)?;
            let green = u8::from_str_radix(&hex[2..4], 16)?;
            let blue = u8::from_str_radix(&hex[4..6], 16)?;
            return Ok(Color::Rgb(red, green, blue));
        }
        bail!("hex colors must be #RRGGBB");
    }

    let color = match value.as_str() {
        "reset" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        other => bail!("unknown color `{other}`"),
    };
    Ok(color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn resolver_loads_project_local_theme_plugin() {
        let temp = TempDir::new().unwrap();
        let notebook = temp.path().join("demo.smd");
        let plugin_dir = temp.path().join(".strata/plugins/ocean");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
id = "ocean"
name = "Ocean"
version = "0.1.0"
capabilities = ["theme"]

[theme]
spec = "theme.toml"
"#,
        )
        .unwrap();
        fs::write(
            plugin_dir.join("theme.toml"),
            r#"
name = "Ocean"

[styles]
"cell.border.selected" = { fg = "green", modifiers = ["bold"] }
"toolbar.button.save" = { fg = "cyan", modifiers = ["bold"] }
"#,
        )
        .unwrap();

        let resolution = ThemeResolver::new(None, None).resolve(Some("ocean"), Some(&notebook));

        assert_eq!(resolution.warning, None);
        assert_eq!(resolution.theme.name(), "Ocean");
        assert_eq!(
            resolution.theme.style("cell.border.selected"),
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        );
    }

    #[test]
    fn resolver_falls_back_to_default_theme_on_missing_plugin() {
        let temp = TempDir::new().unwrap();
        let notebook = temp.path().join("demo.smd");

        let resolution =
            ThemeResolver::new(None, None).resolve(Some("missing-theme"), Some(&notebook));

        assert!(resolution.warning.unwrap().contains("missing-theme"));
        assert_eq!(resolution.theme.name(), "Strata Default");
    }

    #[test]
    fn resolver_discovers_user_plugin_root() {
        let temp = TempDir::new().unwrap();
        let plugin_dir = temp.path().join("strata/plugins/dawn");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
id = "dawn"
name = "Dawn"
version = "0.1.0"
capabilities = ["theme"]

[theme]
spec = "theme.toml"
"#,
        )
        .unwrap();
        fs::write(
            plugin_dir.join("theme.toml"),
            r#"
[styles]
"status.body" = { fg = "yellow" }
"#,
        )
        .unwrap();

        let resolution = ThemeResolver::new(Some(temp.path().to_string_lossy().to_string()), None)
            .resolve(Some("dawn"), None);

        assert_eq!(resolution.warning, None);
        assert_eq!(resolution.theme.name(), "Dawn");
    }
}
