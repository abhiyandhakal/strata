# Strata

Strata is a terminal-native notebook environment with a custom Markdown-based notebook format and a notebook-style TUI.

Current product slice:

- `.smd` as the primary working notebook format
- explicit `.ipynb <-> .smd` conversion commands
- a vertical notebook UI with selection-first interaction
- mouse support for selecting cells, toolbar actions, scrolling, and editor cursor placement
- tree-sitter syntax highlighting for Python, Bash, JavaScript, and TypeScript
- Python LSP activation for Basedpyright / Pyright-compatible servers when available
- checkpoint sidecars for runtime and notebook UI state

## Run

Open a Strata notebook:

```bash
cargo run -- path/to/notebook.smd
```

The same command behaves differently depending on the environment:

- interactive terminal: opens the notebook UI
- non-interactive use: executes the notebook headlessly and prints a summary

## Install As A Command

Install `strata` onto your `PATH`:

```bash
./scripts/install-local.sh
```

By default this installs to `~/.local/bin/strata`.

If `~/.local/bin` is already on `PATH`, you can then run:

```bash
strata path/to/notebook.smd
```

Override the install location if needed:

```bash
STRATA_INSTALL_DIR=/some/bin ./scripts/install-local.sh
```

## Notebook Format

Strata now uses `.smd` as the primary notebook format.

`.smd` is Markdown-based and human-editable, but uses explicit Strata metadata comments so notebook structure round-trips cleanly.

The format stores:

- notebook metadata
- markdown, code, raw, and AI cells
- cell ids
- execution counts
- text outputs and errors where representable

The main workflow is intentionally centered on `.smd`, not `.ipynb`.

## Import And Export

Import an `.ipynb` notebook into Strata format:

```bash
strata import path/to/notebook.ipynb
strata import path/to/notebook.ipynb path/to/notebook.smd
```

Export a Strata notebook to `.ipynb`:

```bash
strata export path/to/notebook.smd
strata export path/to/notebook.smd path/to/notebook.ipynb
```

Direct notebook opening is for `.smd`:

```bash
strata path/to/notebook.smd
```

If you want to work from an `.ipynb`, import it first.

## TUI Workflow

The notebook UI is selection-first.

Cell interaction:

- single click selects a cell
- double click enters edit mode
- `j` / `k` move the selected cell
- `e` or `Enter` enters edit mode for the selected cell
- `Esc` exits edit mode back to cell selection

Selected cells are highlighted as whole notebook cards, not just with an inner border.

Toolbar actions:

- `[Save]`
- `[Run All]`
- `[Restart]`
- `[+ Code]`
- `[+ Markdown]`

Per-cell actions:

- executable cells: `[Run]`, `[Edit]`, `[+]`, `[Del]`, `[Out]`
- markdown cells: `[Edit]` or `[Render]`, `[+]`, `[Del]`, `[Out]` when output exists

Markdown cells do not show a run button.

Keyboard flow:

- `j` / `k`: move selection
- `e` or `Enter`: edit selected cell
- `r`: run selected executable cell
- `R`: run all executable cells
- `c`: insert a Python code cell below
- `m`: insert a Markdown cell below
- `d` or `Delete`: delete selected cell
- `o`: collapse or expand selected cell output
- `Ctrl-S`: save notebook and checkpoint
- `q`: quit

Mouse flow:

- click a cell to select it
- double click a cell body to edit it
- click cell buttons for edit/render, run, insert, delete, and output toggle
- use the scroll wheel to move through notebook cells
- click inside the editor to place the cursor
- drag inside the editor to select text

## Scrolling And Bounds

The notebook viewport is cell-based and clamped so it does not draw content beyond the visible terminal area.

Current behavior:

- notebook scrolling advances through cells safely
- selected cells are kept visible
- long cell bodies and outputs are height-limited in the notebook view
- narrow or short terminals clip safely instead of drawing out of bounds

## Editing And Vim Mode

The notebook editor still supports the optional vim mode for cell editing.

Config:

```toml
[editor]
vim_mode = true
```

Environment overrides:

```bash
export STRATA_CONFIG_PATH=/path/to/config.toml
export STRATA_VIM_MODE=1
```

When vim mode is enabled, entering edit mode starts the cell editor in vim `NORMAL`.

## Syntax Highlighting

Syntax highlighting is powered by tree-sitter grammars for:

- Python
- Bash
- JavaScript
- TypeScript

Markdown cells render as notebook prose when not editing and switch to plain text during editing.

## Python LSP

Strata detects and attempts to activate a Python language server for the notebook UI.

Discovery order:

1. `basedpyright-langserver`
2. `basedpyright`
3. `pyright-langserver`
4. `npx --yes basedpyright-langserver --stdio`

The toolbar reports whether Python LSP is available or active. The current code activates the server process and performs the initialize handshake so the editor can build on a real LSP session.

## Runtime And Checkpoints

Strata keeps runtime state in:

```text
.strata/<notebook-stem>/session.json
```

Checkpoint state includes:

- named values and execution history
- AI history
- next execution counter
- notebook UI state such as selected cell, viewport position, and rendered/collapsed cell modes

Opening an existing notebook rehydrates language runtime state by replaying prior successful code-cell executions.

## Current Scope

Included now:

- `.smd` notebook storage
- `.ipynb` import and export
- notebook-style TUI with selection-first interaction
- markdown cells without run controls
- safer scrolling and viewport clamping
- tree-sitter syntax highlighting
- Python LSP activation path

Still incomplete:

- full Jupyter shortcut parity
- rich MIME output rendering beyond terminal text fallbacks
- mouse drag-reorder for cells
- surfaced LSP UX like completion menus, hover panes, rename, references, and code actions
- full cleanup of older Strata-specific AI and multi-language assumptions in every internal layer

## Verification

```bash
cargo test --quiet
```

That suite covers:

- `.smd` parse/render round-trips
- `.ipynb` parse/render round-trips
- checkpoint persistence
- runtime hydration
- `.smd` notebook execution
- import/export conversion commands
- AI provider integration mocks
- tree-sitter highlighter scaffolding
- notebook TUI editing, selection, and scrolling behavior
