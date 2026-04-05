# Strata

Strata is a terminal-native notebook environment with a custom Markdown-based notebook format and a notebook-style TUI.

Current product slice:

- `.smd` as the primary working notebook format
- explicit `.ipynb <-> .smd` conversion commands
- a vertical notebook UI with selection-first interaction
- mouse support for selecting cells, toolbar actions, scrolling, and editor cursor placement
- notebook-wide kernel and environment selection in the toolbar
- filename fallback for untitled notebooks in the UI
- image-aware outputs with openable image placeholders
- plugin-backed themes with declarative TOML theme specs
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
- `[Kernel: ...]`
- `[Env: ...]`
- `[+ Code]`
- `[+ Markdown]`

Per-cell actions:

- executable cells: `[Run]`, `[Edit]`, `[+]`, `[Del]`, `[Out]`
- markdown cells: `[Edit]` or `[Render]`, `[+]`, `[Del]`, `[Out]` when output exists

Markdown cells do not show a run button.

Keyboard flow:

- `j` / `k`: move selection
- `e` or `Enter`: edit selected cell
- `K`: cycle notebook kernel
- `E`: cycle environment
- `r`: run selected executable cell
- `R`: run all executable cells
- `c`: insert a Python code cell below
- `m`: insert a Markdown cell below
- `d` or `Delete`: delete selected cell
- `o`: collapse or expand selected cell output
- `x`: open the selected cell's first image output externally
- `Ctrl-S`: save notebook and checkpoint
- `q`: quit

Mouse flow:

- click a cell to select it
- double click a cell body to edit it
- click cell buttons for edit/render, run, insert, delete, output toggle, and image open
- use the scroll wheel to move through notebook cells
- click inside the editor to place the cursor
- drag inside the editor to select text

## Notebook Title

If a notebook is still using the default untitled metadata, Strata shows the file stem in the toolbar instead.

That is a display-only fallback. The stored notebook metadata is not silently rewritten.

## Kernel And Environment

Strata now treats the notebook UI as notebook-wide-kernel oriented.

Kernel choices:

- Python
- Bash
- JavaScript

Environment choices:

- `None`
- `System`
- discovered Python environments when the active kernel is Python

Python environment discovery includes:

- the active `VIRTUAL_ENV`
- the active `CONDA_PREFIX`
- notebook-local `.venv`, `venv`, and `env`

Behavior:

- `None` disables code-cell execution for the selected kernel
- `System` uses the default runtime on `PATH`
- discovered Python environments launch the Python kernel with that interpreter

Legacy notebooks without explicit runtime metadata still run in compatibility mode if they contain older mixed-language cells.

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

## Themes

Strata now has a theme plugin system.

Theme selection is config-only in the current version:

```toml
[theme]
path = "nocturne"
```

Theme lookup behavior:

- relative theme paths resolve from the opened notebook directory
- notebook-local theme plugins are discovered under `<notebook-dir>/.strata/plugins/`
- user-level theme plugins are discovered under `~/.config/strata/plugins/`
- if the configured theme is missing or invalid, Strata falls back to the built-in default theme and shows a startup warning in the status area

Theme plugin layout:

```text
my-theme/
  plugin.toml
  theme.toml
```

Example `plugin.toml`:

```toml
id = "nocturne"
name = "Nocturne"
version = "0.1.0"
capabilities = ["theme"]

[theme]
spec = "theme.toml"
```

`theme.toml` is declarative. It maps semantic UI component keys to styles plus syntax token colors.

Example:

```toml
[styles]
"toolbar.block" = { fg = "white", bg = "#08111c" }
"cell.border.selected" = { fg = "lightcyan", modifiers = ["bold"] }
"cell.button.run" = { fg = "lightgreen", modifiers = ["bold"] }
"markdown.heading1" = { fg = "lightyellow", modifiers = ["bold"] }

[syntax]
keyword = { fg = "lightmagenta", modifiers = ["bold"] }
string = { fg = "lightgreen" }
identifier = { fg = "lightblue" }
```

An example theme plugin is included in [examples/theme-plugins/nocturne](examples/theme-plugins/nocturne). Copy it into either `~/.config/strata/plugins/nocturne/` or `<notebook-dir>/.strata/plugins/nocturne/`, then set:

```toml
[theme]
path = "nocturne"
```

The first version of theming supports declarative full-surface skinning for the existing UI, but not executable theme hooks or layout replacement.

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

## Image Outputs

Strata now recognizes image-style outputs in two ways:

- structured notebook outputs such as `display_data` / `execute_result` with image mime types
- focused command/path detection for shell-style output like `display path/to/image.png` or direct image file paths

Current behavior:

- image outputs render as labeled placeholders in the notebook output area
- clicking `[Open]` or pressing `x` opens the first image output for the selected cell with the system default opener
- `.smd` saves materialize imported image payloads into `.strata/<notebook>/artifacts/` and store references instead of embedding large binaries into the notebook source

Supported mime/path families:

- `image/png`
- `image/jpeg`
- `image/svg+xml`
- `image/gif` as external-open fallback

Terminal-native inline image rendering is not the primary path yet. The current shipped behavior is artifact-backed image placeholders plus native system opening.

## Current Scope

Included now:

- `.smd` notebook storage
- `.ipynb` import and export
- notebook-style TUI with selection-first interaction
- notebook-wide kernel/environment controls
- markdown cells without run controls
- safer scrolling and viewport clamping
- image-aware output placeholders and external open actions
- plugin-backed theme loading with a built-in fallback theme
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
- notebook title fallback and runtime metadata round-trips
- environment discovery and legacy-kernel compatibility behavior
- theme plugin discovery and fallback handling
- AI provider integration mocks
- tree-sitter highlighter scaffolding
- notebook TUI editing, selection, and scrolling behavior
