use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, bail};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use serde_json::{Value, json};
use tree_sitter::{Node, Parser, Tree};

use crate::core::Language;
use crate::theme::{SyntaxTokenKind, Theme};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PythonLspStatus {
    Available { command: String, args: Vec<String> },
    Active { command: String },
    Unavailable,
}

impl PythonLspStatus {
    pub fn detect() -> Self {
        for candidate in [
            "basedpyright-langserver",
            "basedpyright",
            "pyright-langserver",
        ] {
            if command_exists(candidate) {
                return Self::Available {
                    command: candidate.to_string(),
                    args: vec!["--stdio".to_string()],
                };
            }
        }

        if command_exists("node") {
            return Self::Available {
                command: "npx".to_string(),
                args: vec![
                    "--yes".to_string(),
                    "basedpyright-langserver".to_string(),
                    "--stdio".to_string(),
                ],
            };
        }

        Self::Unavailable
    }

    pub fn summary(&self) -> String {
        match self {
            PythonLspStatus::Available { command, .. } => format!("python lsp via {command}"),
            PythonLspStatus::Active { command } => format!("python lsp active via {command}"),
            PythonLspStatus::Unavailable => "python lsp unavailable".to_string(),
        }
    }
}

pub struct PythonLspClient {
    command: String,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl PythonLspClient {
    pub fn activate(status: &PythonLspStatus) -> Result<(Self, PythonLspStatus)> {
        let (command, args) = match status {
            PythonLspStatus::Available { command, args } => (command.clone(), args.clone()),
            PythonLspStatus::Active { .. } => bail!("python lsp already active"),
            PythonLspStatus::Unavailable => bail!("python lsp unavailable"),
        };

        let mut child = Command::new(&command)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to start python lsp command {command}"))?;
        let stdin = child.stdin.take().context("python lsp stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("python lsp stdout unavailable")?;
        let mut client = Self {
            command: command.clone(),
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };
        client.initialize()?;

        Ok((client, PythonLspStatus::Active { command }))
    }

    fn initialize(&mut self) -> Result<()> {
        let initialize_id = self.next_request_id();
        self.send_request(
            initialize_id,
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": Value::Null,
                "capabilities": {},
                "clientInfo": {
                    "name": "strata",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )?;
        let _ = self.read_message()?;
        self.send_notification("initialized", json!({}))?;
        Ok(())
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn send_request(&mut self, id: u64, method: &str, params: Value) -> Result<()> {
        self.send_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
    }

    fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        self.send_message(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn send_message(&mut self, value: Value) -> Result<()> {
        let body = serde_json::to_vec(&value)?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len())?;
        self.stdin.write_all(&body)?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_message(&mut self) -> Result<Value> {
        let mut content_length = None;
        loop {
            let mut header = String::new();
            let read = self.stdout.read_line(&mut header)?;
            if read == 0 {
                bail!("python lsp closed stdout");
            }
            if header == "\r\n" {
                break;
            }
            if let Some(value) = header.strip_prefix("Content-Length:") {
                content_length = Some(value.trim().parse::<usize>()?);
            }
        }
        let content_length = content_length.context("missing content length from python lsp")?;
        let mut body = vec![0u8; content_length];
        self.stdout.read_exact(&mut body)?;
        let value = serde_json::from_slice(&body)?;
        Ok(value)
    }

    pub fn shutdown(&mut self) -> Result<()> {
        let shutdown_id = self.next_request_id();
        let _ = self.send_request(shutdown_id, "shutdown", json!({}));
        let _ = self.read_message();
        let _ = self.send_notification("exit", json!({}));
        let _ = self.child.wait();
        Ok(())
    }

    pub fn command(&self) -> &str {
        &self.command
    }
}

impl Drop for PythonLspClient {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Clone, Debug)]
pub struct SyntaxHighlighter;

impl SyntaxHighlighter {
    pub fn highlight(language: Language, source: &str) -> Text<'static> {
        Self::highlight_with_theme(language, source, &Theme::default_theme())
    }

    pub fn highlight_with_theme(language: Language, source: &str, theme: &Theme) -> Text<'static> {
        match parse_tree(language, source) {
            Some(tree) => render_tree(language, source, &tree, theme),
            None => Text::from(
                source
                    .lines()
                    .map(|line| Line::from(line.to_string()))
                    .collect::<Vec<_>>(),
            ),
        }
    }
}

#[derive(Clone, Debug)]
struct HighlightSpan {
    start: usize,
    end: usize,
    style: Style,
}

fn parse_tree(language: Language, source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    let grammar = match language {
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::Bash => tree_sitter_bash::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        _ => return None,
    };
    parser.set_language(&grammar).ok()?;
    parser.parse(source, None)
}

fn render_tree(language: Language, source: &str, tree: &Tree, theme: &Theme) -> Text<'static> {
    let mut spans = Vec::new();
    collect_leaf_spans(language, tree.root_node(), theme, &mut spans);
    spans.sort_by_key(|span| (span.start, span.end));

    let mut lines = Vec::new();
    let mut offset = 0usize;
    for raw_line in source.split_inclusive('\n') {
        let line_start = offset;
        let line_end = offset + raw_line.len();
        let mut cursor = line_start;
        let mut line_spans = Vec::new();

        for span in spans
            .iter()
            .filter(|span| span.end > line_start && span.start < line_end)
        {
            let start = span.start.max(line_start);
            let end = span.end.min(line_end);
            if start > cursor {
                line_spans.push(Span::raw(source[cursor..start].to_string()));
            }
            if end > start {
                line_spans.push(Span::styled(source[start..end].to_string(), span.style));
            }
            cursor = end;
        }

        if cursor < line_end {
            line_spans.push(Span::raw(source[cursor..line_end].to_string()));
        }
        if line_spans.is_empty() {
            line_spans.push(Span::raw(raw_line.to_string()));
        }
        lines.push(Line::from(line_spans));
        offset = line_end;
    }

    if lines.is_empty() {
        lines.push(Line::from(String::new()));
    }

    Text::from(lines)
}

fn collect_leaf_spans(
    language: Language,
    node: Node<'_>,
    theme: &Theme,
    spans: &mut Vec<HighlightSpan>,
) {
    if node.child_count() == 0 {
        if let Some(style) = style_for_node(language, node.kind(), theme) {
            spans.push(HighlightSpan {
                start: node.start_byte(),
                end: node.end_byte(),
                style,
            });
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_leaf_spans(language, child, theme, spans);
    }
}

fn style_for_node(language: Language, kind: &str, theme: &Theme) -> Option<Style> {
    let style = if kind.contains("comment") {
        theme.syntax_style(SyntaxTokenKind::Comment)
    } else if kind.contains("string") || kind == "string_fragment" {
        theme.syntax_style(SyntaxTokenKind::String)
    } else if kind.contains("number") || kind == "integer" || kind == "float" {
        theme.syntax_style(SyntaxTokenKind::Number)
    } else if kind.contains("type") || kind == "predefined_type" {
        theme.syntax_style(SyntaxTokenKind::TypeName)
    } else if is_keyword(language, kind) {
        theme.syntax_style(SyntaxTokenKind::Keyword)
    } else if kind.contains("function")
        || kind == "identifier"
        || kind == "property_identifier"
        || kind == "variable_name"
    {
        theme.syntax_style(SyntaxTokenKind::Identifier)
    } else {
        return None;
    };

    Some(style)
}

fn is_keyword(language: Language, kind: &str) -> bool {
    match language {
        Language::Python => matches!(
            kind,
            "def"
                | "class"
                | "import"
                | "from"
                | "return"
                | "if"
                | "else"
                | "elif"
                | "for"
                | "while"
                | "try"
                | "except"
                | "with"
                | "as"
                | "lambda"
                | "await"
                | "async"
                | "in"
                | "not"
                | "and"
                | "or"
        ),
        Language::Bash => matches!(
            kind,
            "if" | "then" | "else" | "fi" | "for" | "do" | "done" | "while" | "function"
        ),
        Language::JavaScript | Language::TypeScript => matches!(
            kind,
            "function"
                | "return"
                | "if"
                | "else"
                | "for"
                | "while"
                | "const"
                | "let"
                | "var"
                | "class"
                | "async"
                | "await"
                | "import"
                | "export"
                | "from"
                | "extends"
        ),
        _ => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_sitter_highlighter_styles_python_keywords() {
        let highlighted =
            SyntaxHighlighter::highlight(Language::Python, "def add(x):\n    return x");
        let rendered = format!("{highlighted:?}");

        assert!(rendered.contains("def"));
        assert!(rendered.contains("return"));
    }

    #[test]
    fn tree_sitter_highlighter_accepts_custom_theme() {
        let theme = Theme::default_theme();
        let highlighted =
            SyntaxHighlighter::highlight_with_theme(Language::Python, "value = 'hi'", &theme);

        let rendered = format!("{highlighted:?}");
        assert!(rendered.contains("hi"));
    }

    #[test]
    fn python_lsp_status_reports_summary() {
        let summary = PythonLspStatus::detect().summary();
        assert!(summary.contains("python lsp"));
    }
}
