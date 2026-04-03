# Strata

Strata is a terminal-native, structured execution environment for stateful notebooks in the CLI.

This repository now contains the first real execution milestone of the architecture:

- Markdown-backed notebooks with text, code, and AI cells
- Checkpoint sidecar storage for resumable session metadata and named values
- Managed worker-backed kernels for Bash, Python, JavaScript, and TypeScript
- Provider-based AI interfaces with automatic context selection
- A minimal `ratatui` notebook viewer/editor shell
- A CLI execution path that runs notebooks and persists checkpoints

## Run

```bash
cargo run
```

Or open a notebook:

```bash
cargo run -- open path/to/notebook.md
```

Run a notebook end-to-end:

```bash
cargo run -- run path/to/notebook.md
```

## Current scope

The runtime now executes real child-process workers and supports cross-language named-value handoff. The TUI is still minimal, AI execution is still provider-scaffolding rather than a real remote integration, and checkpoint resume does not yet rebuild full in-memory language state across app restarts. The project structure is ready for richer editing, provider-backed AI execution, deeper checkpoint hydration, and future plugin work.
