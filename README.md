# Strata

Strata is a terminal-native, structured execution environment for stateful notebooks in the CLI.

This repository now contains a much larger product slice of the architecture:

- Markdown-backed notebooks with text, code, and AI cells
- Checkpoint sidecar storage for resumable session metadata, execution history, AI history, and named values
- Managed worker-backed kernels for Bash, Python, JavaScript, and TypeScript
- Real AI provider integration for OpenAI Responses API and Anthropic Messages API
- `models.dev` model catalog integration for provider/model lookup
- A keyboard-first `ratatui` notebook editor with inline execution
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

## AI setup

Strata loads AI provider credentials from environment variables.

OpenAI:

```bash
export OPENAI_API_KEY=...
export STRATA_AI_PROVIDER=openai
# optional
export STRATA_AI_MODEL=...
```

Anthropic:

```bash
export ANTHROPIC_API_KEY=...
export STRATA_AI_PROVIDER=anthropic
# optional
export STRATA_AI_MODEL=...
```

If `STRATA_AI_PROVIDER` is unset, Strata picks the first configured provider. If `STRATA_AI_MODEL` is unset, Strata resolves a text-capable model from the live `models.dev` catalog.

## TUI workflow

When opening a notebook in a real terminal, Strata launches the TUI. Current key workflow:

- `j` / `k`: move between cells
- `v`: toggle vim mode for the current TUI session
- `e`: edit selected cell
- `Esc`: leave edit mode and keep changes in memory
- `Ctrl-S`: save the notebook and checkpoint
- `Ctrl-R`: run the current cell while editing
- `r`: run the selected cell in normal mode
- `b` / `p` / `J` / `t` / `a` / `n`: insert Bash, Python, JavaScript, TypeScript, AI, or text cells
- `x`: delete the selected cell
- `q`: quit

The right pane shows the latest output or error for the selected cell, including AI responses and provider/model metadata when available.

## Vim mode

Strata now supports an opt-in vim editor mode for the cell editor only. Notebook navigation outside editing keeps the regular Strata keymap.

Default configuration can come from TOML:

```toml
# ~/.config/strata/config.toml
[editor]
vim_mode = true
```

Override config file discovery if needed:

```bash
export STRATA_CONFIG_PATH=/path/to/config.toml
```

Override vim mode directly from the environment:

```bash
export STRATA_VIM_MODE=1
```

When vim mode is enabled and you press `e`, the editor starts in vim `NORMAL` mode. Key behavior:

- `i`, `a`, `A`, `I`, `o`, `O`: enter insert mode
- `Esc`: return to vim `NORMAL`; pressing `Esc` again in vim `NORMAL` exits editor mode back to notebook mode
- `h`, `j`, `k`, `l`, `w`, `b`, `e`, `0`, `^`, `$`: movement
- `v`, `V`: visual selection
- `x`, `dd`, `yy`, `p`, `u`, `Ctrl-R`: common editing operations

Notebook-level `q`, `r`, insert-cell shortcuts, and save/run bindings remain outside the vim editor scope.

## Resume behavior

Strata stores checkpoints under `.strata/<notebook-stem>/`. On reopen or `run`, it reloads the checkpoint and rehydrates runtime state by replaying previously successful code-cell executions into the managed kernels. Named values and AI history are restored from the checkpoint manifest.

## Current scope

The runtime now executes real child-process workers, supports cross-language named-value handoff, provides real remote AI calls, and restores kernel state on resume by replaying prior successful cells. The next major gaps are richer editor ergonomics, AI patch/apply workflows, plugin support, dependency-aware execution, remote execution targets, and agent workflows.
