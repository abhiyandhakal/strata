# Strata

Strata is now being reshaped into a terminal-native notebook that deliberately follows the Jupyter notebook model and interaction style.

Current product slice:

- `.ipynb`-first notebook storage with nbformat-style cells and outputs
- Python-first notebook execution with inline execution counts and output blocks
- A vertical notebook TUI instead of the older split-pane editor
- Mouse support for toolbar actions, cell focus, per-cell buttons, scrolling, and editor cursor placement
- Tree-sitter-backed syntax highlighting for Python, Bash, JavaScript, and TypeScript
- Python LSP activation path for Basedpyright / Pyright-style servers when available
- Checkpoint sidecars for resumable runtime state and notebook UI state

## Run

Open the notebook UI:

```bash
cargo run -- open path/to/notebook.ipynb
```

Run a notebook headlessly:

```bash
cargo run -- run path/to/notebook.ipynb
```

If no path is provided, Strata opens an in-memory demo notebook.

## Install As A Command

To install `strata` so it can be run from anywhere without `cargo run` or a binary path:

```bash
./scripts/install-local.sh
```

That builds the release binary and installs it to `~/.local/bin/strata` by default.

Override the install location if needed:

```bash
STRATA_INSTALL_DIR=/some/bin ./scripts/install-local.sh
```

If your shell already has `~/.local/bin` on `PATH`, you can then run:

```bash
strata --help
strata open path/to/notebook.ipynb
```

## Notebook Format

Strata now treats `.ipynb` as the primary notebook format.

Supported visible cell types:

- `code`
- `markdown`
- `raw`

Code-cell execution results are written back into the notebook as:

- `execution_count`
- structured outputs
- error output blocks

Markdown-backed Strata notebooks still load for compatibility, but the notebook redesign is centered on `.ipynb`.

## TUI Workflow

When launched in a real terminal, Strata opens a notebook-style document view:

- top toolbar with save, run-all, restart, and insert-cell actions
- vertically stacked cells
- per-cell chrome with run / render / insert / delete / output toggle controls
- inline outputs directly below each code cell

Keyboard flow:

- `j` / `k`: move cell focus
- `e` or `Enter`: edit focused cell
- `r`: run focused cell
- `R`: run all executable cells
- `c`: insert a Python code cell below
- `m`: insert a Markdown cell below
- `d` or `Delete`: delete focused cell
- `o`: collapse or expand focused cell output
- `Ctrl-S`: save notebook and checkpoint
- `q`: quit

Mouse flow:

- click a cell to select it
- click cell toolbar buttons to run, toggle render, insert, delete, or collapse output
- click toolbar buttons for save, run-all, restart, and insert-cell actions
- use the scroll wheel to move through the notebook
- click inside the editor to position the cursor
- drag in the editor to select text

## Editing and Vim Mode

The document UI still supports the existing editor-mode vim toggle.

Config:

```toml
# ~/.config/strata/config.toml
[editor]
vim_mode = true
```

Environment overrides:

```bash
export STRATA_CONFIG_PATH=/path/to/config.toml
export STRATA_VIM_MODE=1
```

With vim mode enabled, entering edit mode starts the cell editor in vim `NORMAL`.

## Syntax Highlighting

Syntax highlighting is now driven by tree-sitter grammars for:

- Python
- Bash
- JavaScript
- TypeScript

Markdown cells render as formatted notebook content when not editing and switch back to plain text while editing.

## Python LSP

Strata now detects and attempts to activate a Python language server for the notebook UI.

Discovery order:

1. `basedpyright-langserver`
2. `basedpyright`
3. `pyright-langserver`
4. `npx --yes basedpyright-langserver --stdio`

The toolbar reports whether Python LSP is available or active. The current code activates the server process and performs the LSP initialize handshake so the UI can build on a real server connection.

## Runtime and Checkpoints

Strata keeps runtime state in a sidecar checkpoint directory:

```text
.strata/<notebook-stem>/session.json
```

Checkpoint state currently includes:

- named values and execution history
- AI history
- next execution counter
- notebook UI state such as selected cell, viewport, and rendered/collapsed cell modes

Opening an existing notebook rehydrates language runtime state by replaying prior successful code-cell executions.

## Current Scope

This redesign is intentionally Jupyter-shaped first.

Included now:

- `.ipynb` model and persistence
- notebook-style TUI
- inline outputs and execution counts
- mouse-driven notebook interactions
- tree-sitter syntax highlighting
- Python LSP activation path

Still incomplete:

- full Jupyter shortcut parity
- rich MIME output rendering beyond text-first terminal fallbacks
- notebook-wide drag-reorder via mouse
- surfaced LSP UX like completion popups, hover panes, rename, references, and code actions
- full removal of older Strata-specific AI and multi-language notebook assumptions from every internal layer

## Verification

Current verification commands:

```bash
cargo test --quiet
```

That suite covers:

- Markdown and `.ipynb` storage round-trips
- checkpoint persistence
- runtime hydration
- CLI notebook execution
- AI provider integration mocks
- tree-sitter highlighter scaffolding
- notebook TUI editing and execution flows
