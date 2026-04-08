# Strata

Strata is a terminal-first notebook for Python-heavy exploratory work.

Today, the product is best understood as:

- a notebook-style TUI for code and markdown cells
- a human-editable `.smd` notebook format
- Jupyter `.ipynb` import/export
- notebook-wide kernel and environment selection
- checkpointed runtime state under `.strata/`

It is not a general IDE, and it is not a broad AI platform yet.

## What It Is Aimed At

Strata is currently aimed at people who want a notebook workflow without leaving the terminal:

- exploratory Python work
- research notes mixed with runnable code
- quick iteration on local data/scripts
- a notebook UI that stays close to plain text and git

The current experience is Python-first. Bash and JavaScript kernels exist, but the main notebook story is still centered on Python notebooks with markdown.

## Current Product Slice

Shipped now:

- `.smd` as the primary working notebook format
- `.ipynb <-> .smd` conversion commands
- a notebook-style TUI with selection-first interaction
- command mode and edit mode
- mouse support for selection, buttons, scrolling, and editor cursor placement
- notebook-wide kernel and environment controls
- asynchronous execution with a busy state
- `In [*]:` while a code cell is running
- blocking of overlapping runs while execution is in progress
- tree-sitter syntax highlighting for Python, Bash, JavaScript, and TypeScript
- Python LSP activation for Basedpyright / Pyright-compatible servers when available
- markdown image rendering with inline display when possible and clickable fallback links otherwise
- image-aware outputs with external open actions
- theme plugins with declarative TOML theme specs
- checkpoint sidecars for runtime and notebook UI state

Not the main focus right now:

- AI cells as a primary workflow
- equal maturity across every language/runtime
- rich LSP UI like completion menus, hover panes, rename, and code actions
- interrupt/stop for running cells
- broad plugin extensibility beyond themes

## Install And Run

Install the binary onto your `PATH`:

```bash
./scripts/install-local.sh
```

By default this installs to `~/.local/bin/strata`.

Then open a notebook from anywhere:

```bash
strata path/to/notebook.smd
```

Behavior depends on the environment:

- interactive terminal: opens the notebook UI
- non-interactive use: executes the notebook headlessly and prints a summary

Override the install location if needed:

```bash
STRATA_INSTALL_DIR=/some/bin ./scripts/install-local.sh
```

## Notebook Workflow

Open a notebook:

```bash
strata notes.smd
```

Toolbar actions:

- `[Save]`
- `[Run All]`
- `[Restart]`
- `[Kernel: ...]`
- `[Env: ...]`
- `[+ Code]`
- `[+ Markdown]`

Per-cell actions:

- code cells: `[Run]`, `[Edit]`, `[+]`, `[Fold]`, `[Del]`, output toggle when output exists
- markdown cells: `[Render]` or `[Edit]`, `[+]`, `[Fold]`, `[Del]`

Markdown cells do not have a run button.

Command-mode keys:

- `j` / `k`: move selection
- `e` or `Enter`: edit selected cell
- `r`: run selected executable cell
- `R`: run all executable cells
- `K`: cycle kernel
- `E`: cycle environment
- `c`: insert code cell below
- `m`: insert markdown cell below
- `d` or `Delete`: delete selected cell
- `z`: fold or unfold the selected cell body
- `o`: collapse or expand selected cell output
- `y`: copy current target
- `Y`: copy selected cell block
- `gy`: copy selected cell output
- `x`: open the selected cell’s first image
- `Ctrl-S`: save
- `q`: quit
- `Esc`: clear selection

Mouse behavior:

- single click selects a cell
- double click on a cell body enters edit mode
- click toolbar and cell buttons directly
- mouse wheel scrolls the notebook
- drag in rendered text/output to select text for copying
- click in the editor to place the cursor

Execution behavior:

- only one execution job runs at a time
- while a cell is running, its prompt shows `In [*]:`
- overlapping `Run` / `Run All` actions are blocked
- the UI stays responsive for scrolling and inspection while execution is active

## File Formats

### `.smd`

`.smd` is Strata’s primary working format.

It is Markdown-based and human-editable, with explicit Strata metadata comments so notebook structure round-trips cleanly.

The format stores:

- notebook metadata
- markdown, code, raw, and AI cells
- cell ids
- execution counts
- outputs and errors where representable

### `.ipynb`

`.ipynb` is supported as an interchange format.

Import:

```bash
strata import path/to/notebook.ipynb
strata import path/to/notebook.ipynb path/to/notebook.smd
```

Export:

```bash
strata export path/to/notebook.smd
strata export path/to/notebook.smd path/to/notebook.ipynb
```

Direct opening is for `.smd` notebooks:

```bash
strata path/to/notebook.smd
```

## Kernels And Environments

Notebook-wide kernels:

- Python
- Bash
- JavaScript

Environment choices:

- `None`
- `System`
- discovered Python environments when the active kernel is Python

Python environment discovery includes:

- active `VIRTUAL_ENV`
- active `CONDA_PREFIX`
- notebook-local `.venv`, `venv`, and `env`

Behavior:

- `None` disables execution for code cells under that kernel
- `System` uses the default runtime on `PATH`
- discovered Python environments launch the Python kernel with that interpreter

The current notebook UX is still Python-first even though Bash and JavaScript are available.

## Editing, Vim Mode, And Themes

Optional vim mode applies only inside the editor, not to notebook-level navigation.

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

When vim mode is enabled, entering edit mode starts the cell editor in vim `NORMAL`. `:w` is supported for save.

Themes are configured through plugin directories:

```toml
[theme]
path = "nocturne"
```

Theme lookup:

- `~/.config/strata/plugins/`
- `<notebook-dir>/.strata/plugins/`

If the configured theme is missing or invalid, Strata falls back to the built-in default theme.

An example theme is included in [examples/theme-plugins/nocturne](examples/theme-plugins/nocturne).

## Images And Outputs

Markdown image references like `![alt](./image.svg)` are supported in rendered markdown cells.

Current behavior:

- inline image rendering when terminal support is available
- otherwise underlined clickable alt text
- if the file is missing, plain alt text only

Image-like execution outputs are also recognized.

Current output behavior:

- image outputs render as labeled notebook output blocks
- clicking `[Open]` or pressing `x` opens the first image for the selected cell
- `.smd` saves materialize imported image payloads into `.strata/<notebook>/artifacts/`

## Checkpoints And `.strata`

Strata keeps runtime state in a sidecar directory:

```text
.strata/<notebook-stem>/
```

That includes things like:

- `session.json`
- execution history
- named values
- UI state
- materialized artifacts

The `.smd` file is the source of truth for notebook content. `.strata/` is generated runtime/checkpoint state and should usually be gitignored:

```gitignore
.strata/
```

## Current Limitations

- no interrupt/stop button for a running cell yet
- Python LSP is activated, but rich LSP UI is still incomplete
- Bash and JavaScript support exist, but the UX is not equally polished across all kernels
- AI support exists in the codebase, but it is not the primary documented workflow
- the theme plugin system is the main shipped plugin surface right now

## Verification

```bash
cargo test --quiet
```

That suite currently covers:

- `.smd` round-trips
- `.ipynb` parse/export behavior
- checkpoint persistence and hydration
- notebook execution
- busy execution state and blocked overlapping runs
- theme plugin discovery and fallback
- syntax highlighting and editor behavior
- notebook TUI editing, selection, folding, scrolling, and image handling
