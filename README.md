# Strata

Strata is a terminal-native, structured execution environment for stateful notebooks in the CLI.

This repository now contains the first implementation slice of the architecture:

- Markdown-backed notebooks with text, code, and AI cells
- Checkpoint sidecar storage for resumable session metadata
- Managed kernel interfaces plus prototype Bash and Python adapters
- Provider-based AI interfaces with automatic context selection
- A minimal `ratatui` notebook viewer/editor shell

## Run

```bash
cargo run
```

Or open a notebook:

```bash
cargo run -- path/to/notebook.md
```

## Current scope

The current runtime is a prototype implementation of the planned system shape. The kernel adapters are stateful and testable, but intentionally lightweight rather than full embedded interpreters. The project structure is ready for deeper execution backends, richer TUI editing, and real AI provider integrations.
