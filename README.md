# loom

loom executes Julia, R, and Python code written in Typst documents and lets
Typst typeset the results. Code cells are ordinary Typst code blocks with
Quarto-style options. loom runs them in persistent kernels, stores the
results under `.loom/`, and a small Typst library renders them. Typst itself
never runs code, so the document compiles at any time and Tinymist's live
preview keeps working while you write.

````typst
#import ".loom/loom.typ": loom
#show: loom

```julia
#| label: fig-sine
#| fig-cap: A sine wave on $[0, 2 pi]$.
using Plots
plot(sin, 0, 2π; label = "")
```

@fig-sine shows one period; its maximum is `{julia} maximum(sin.(0:0.01:2π))`.
````

The manual, `docs/manual/`, is itself a loom document; build it with

```sh
loom render docs/manual/manual.typ -- docs/manual/loom-manual.pdf
```

after creating its environments: in `docs/manual/`, run
`julia --project=. -e 'using Pkg; Pkg.instantiate()'`,
`Rscript -e 'renv::restore()'`, and
`python3 -m venv .venv && .venv/bin/pip install -r requirements.txt`.

## Install

```sh
cargo install --path .
```

The `loom` binary contains the kernels and the Typst library; `loom` writes
them into the project when needed.

Requirements:

- Typst 0.15. loom locates cells with the parser of Typst 0.15, and the Typst
  library must agree with it on the text of each code block.
- Julia 1.10 or later, for Julia cells. No packages are required in your
  environment.
- R 4.0 or later with `jsonlite` and `evaluate`, for R cells. `svglite` is
  used for SVG figures when installed.
- Python 3.9 or later, for Python cells. Figures need matplotlib.

## Quick start

```sh
loom init                 # writes .loom/loom.typ, .loom/cells.json, loom.toml
loom run paper.typ        # executes the cells
typst compile paper.typ   # or open paper.typ with Tinymist
```

For live preview, run `loom watch paper.typ` in a terminal and open the file
in an editor with Tinymist. Each time you save, loom executes what changed and
the preview updates.

`loom render paper.typ` runs the cells and then compiles in strict mode, where
any cell without a current result is an error. Use it for the final PDF.

## Cells

A fenced raw block whose language tag is `julia`, `r`, or `python` is a cell.
Options go at the top, one per line, as `#| key: value`:

````typst
```r
#| echo: false
#| fig-width: 6
library(ggplot2)
ggplot(mtcars, aes(wt, mpg)) + geom_point()
```
````

Inline code is written as `` `{julia} expr` ``, `` `{r} expr` ``, or
`` `{python} expr` ``; the result replaces it in the text.

A code block with `#| eval: false` is shown but not executed. Code blocks in
other languages are left alone.

### What a cell shows

Each statement's code is followed by its output: printed text, warnings and
messages, plots, and the value of the cell.

- Julia and Python show the value of the last statement, as Jupyter does. A
  trailing `;` hides it. With `prompt: true` the cell is shown as a REPL
  session, and the value of every statement appears.
- R shows every visible value, as knitr does.
- A value that can be shown as an image (a Plots.jl or Makie figure, a ggplot,
  a matplotlib figure) becomes a figure. Several plots in one cell become one
  figure with several images.
- A value with a Typst representation becomes Typst content: in Julia,
  `show(io, MIME"text/typst"(), x)` (SummaryTables.jl); in R, tinytable
  objects, gt objects if the installed gt has `as_typst()`, or your own
  `loom_typst()` method; in Python, `_repr_typst_()`.

### Options

Options marked *exec* change what the kernel computes: changing one re-executes
the cell. The others only change how the stored result is drawn, so editing
them updates the preview immediately.

| Option | | Default | Meaning |
|---|---|---|---|
| `eval` | | `true` | execute the cell |
| `session` | exec | the chapter | name of the session the cell runs in |
| `setup` | exec | `false` | run this cell at the start of every session of its language |
| `stateless` | exec | `false` | the cell creates and rebinds no variables; see below |
| `import`, `export` | exec | none | names of values read from or written for other cells |
| `error` | exec | `false` | an error is expected output instead of a failure |
| `fig-width`, `fig-height` | exec | 7, 5 | figure size in inches |
| `fig-format` | exec | `svg` | `svg`, `png`, or `pdf` |
| `fig-dpi` | exec | 300 | resolution of raster figures |
| `echo` | | `true` | show the code |
| `output` | | `true` | `false` hides the output; `asis` treats it as Typst markup |
| `include` | | `true` | `false` hides code and output (the cell still runs) |
| `warning`, `message` | | `true` | show warnings, messages |
| `prompt` | | `false` | REPL style, with `julia>`, `>`, or `>>>` prompts |
| `collapse` | | `false` | code and text output in one box (implied by `prompt`) |
| `comment` | | none | prefix for each line of text output, e.g. `##` |
| `label` | | none | `fig-…`, `tbl-…`, or `eq-…`; see below |
| `fig-cap`, `tbl-cap` | | none | caption, written in Typst markup |
| `fig-subcap` | | none | captions of the subfigures, as a list |
| `out-width` | | `fig-width` | displayed width, e.g. `80%` or `3in` |
| `column` | | `body` | `margin` or `page`; see below |
| `layout-ncol` | | 1 | number of columns for the figures of a cell |

Project-wide defaults for `fig-width`, `fig-height`, `fig-format`, `fig-dpi`,
and `error` go in `loom.toml`. Document-wide defaults for `echo`, `output`,
`warning`, `message`, `prompt`, `collapse`, and `comment` are arguments of
`loom`: `#show: loom.with(echo: false)`.

### Figures, tables, and cross-references

loom produces standard Typst figures, so they are numbered together with the
document's other figures and styled by its `figure` rules.

- `label: fig-name` or `fig-cap` puts the cell's images in a
  `figure(kind: image)`; reference it as `@fig-name`.
- `label: tbl-name` or `tbl-cap` puts Typst output in a `figure(kind: table)`.
- `label: eq-name` with `output: asis` labels an equation produced by the
  cell, e.g. a string `"$ y = X beta $"`.
- `fig-subcap` makes each image of the cell a subfigure, lettered (a), (b),
  …, with its own caption. With `label: fig-name`, the subfigures are
  `@fig-name-1`, `@fig-name-2`, which read "Figure 3a", "Figure 3b". The
  list is written as in Quarto, over several lines (`#| fig-subcap:` then
  `#|   - "First"` lines) or on one line (`fig-subcap: ["First", "Second"]`).

Captions and Typst output are evaluated with `eval`, which does not see your
document's definitions. Pass the ones they need with
`#show: loom.with(scope: (name: value))`.

`column: margin` and `column: page` place the figure (or the whole output)
with a function supplied by the document, so the template decides what a
margin is:

```typst
#import "@preview/marginalia:0.3.1" as marginalia
#show: loom.with(columns: (
  margin: body => marginalia.note(numbering: none, body),
  page: body => marginalia.wideblock(body),
))
```

### Style

Code is shown as a `raw` block, so `show raw` rules apply to it. The boxes
around code, output, and errors are set with
`#show: loom.with(style: (code: (fill: luma(240)), output: (color: gray)))`;
see the defaults at the top of `.loom/loom.typ`.

## Sharing data between languages

Each language has its own kernel, so variables do not cross languages by
themselves. Two options move data between cells of any language or session:

````typst
```r
#| export: cars
cars <- mtcars[, c("mpg", "wt", "hp")]
```

```julia
#| import: cars
#| export: beta
X = hcat(ones(size(cars, 1)), cars.wt, cars.hp)
beta = X \ cars.mpg
```
````

An import reads the latest earlier export of that name. Data frames, arrays
(of numbers, logicals, or strings, any shape), and single values can be
shared; objects with a class of their own cannot. Values travel as Arrow
files, which needs `nanoarrow` in R, `pyarrow` in Python, and Arrow.jl (with
DataFrames.jl for data frames) in Julia. Each language runs on its own
thread; a cell waits until the data it imports exists.

## Books and sessions

Each `#include` in the root file starts a chapter, named after the included
file. Every chapter gets its own session per language, so variables do not
leak between chapters. Cells with the same `session:` name share a session
across chapters, in document order. Setup cells (`#| setup: true`, or any cell
in a file named `preamble.typ`) run first and are replayed at the start of
every session of their language.

## Live preview

Tinymist recompiles on every keystroke, before you save; loom only sees saved
files. The Typst library therefore checks, for every cell, that its stored
result still belongs to the document on screen:

- a cell you are typing in has no result yet and shows **not executed
  yet**; a cell with a `label` keeps showing its last result, marked
  **stale**, so figures do not disappear while you edit them;
- cells after an edited, inserted, or deleted cell of the same session are
  marked **stale**, because they ran on a state that no longer exists;
- a cell whose code raised an error shows the error.

Saving runs what changed. During `loom watch`, results appear as each cell
finishes. Saving while cells are running affects the run only if it changes
cells: if the cell being computed was edited, it is interrupted; if other
cells changed, the run stops after the current cell; then loom starts over.
Saving prose leaves the run alone. Ctrl-C interrupts the cell being computed
and keeps watching; a second Ctrl-C, or one while nothing runs, quits.
`loom render` (or `typst compile --input loom=strict`) refuses to compile
while any cell is in one of these states.

### Zed with the typst-live extension

[typst-live](https://github.com/gragusa/typst-live) previews a document in
the browser from Zed's unsaved buffers, through its own Tinymist process.
loom runs next to it as a Zed task: the preview shows your text as you type,
and loom supplies the results of the code each time a file is saved.

1. In the directory of the root file, run `loom init` once and enable loom in
   the root file (`#import ".loom/loom.typ": loom` and `#show: loom`, after
   your template and text settings).

2. Add two tasks to the project's task file, `.zed/tasks.json` inside the
   folder you open in Zed (`$ZED_WORKTREE_ROOT` below); create the `.zed`
   directory and the file if they do not exist, and keep any tasks already
   there. Replace `book.typ` with your root file, giving its path relative to
   that folder:

   ```json
   [
     {
       "label": "loom: watch",
       "command": "loom",
       "args": ["watch", "$ZED_WORKTREE_ROOT/book.typ"],
       "cwd": "$ZED_WORKTREE_ROOT",
       "save": "none",
       "reveal": "no_focus"
     },
     {
       "label": "Typst: Synchronized Main Preview",
       "command": "zed-typst-preview",
       "args": ["start", "--root", "$ZED_WORKTREE_ROOT", "$ZED_WORKTREE_ROOT/book.typ"],
       "save": "none",
       "allow_concurrent_runs": true,
       "reveal": "never"
     }
   ]
   ```

   The second task fixes the preview's entry point to the root file, so that
   chapters are compiled with the root file's `#show: loom` while you edit
   them. For a single-file document, typst-live's own
   **Typst: Synchronized Preview** task, started from that file, is enough.

3. Run **task: spawn** → **loom: watch**, then **task: spawn** →
   **Typst: Synchronized Main Preview**. The first run of `loom watch`
   executes every stale cell; its progress and errors are in the task's
   terminal (`` Ctrl+` ``).

While you type, prose, captions, and display options (`echo`, `fig-cap`,
`layout-ncol`, …) update in the preview at once. A code cell you are
editing is marked **not executed yet** (or keeps its last figure, marked
**stale**, if it has a label) until you save; loom then runs the cells that
changed and rewrites `.loom/cells.json`, which the preview's Tinymist
reloads. In the `loom: watch` terminal, Ctrl-C interrupts a long cell;
closing the terminal tab stops loom and its kernels.

To run code without saving by hand, let Zed save after a pause, in
`.zed/settings.json`:

```json
{ "autosave": { "after_delay": { "milliseconds": 1500 } } }
```

Saves that change only prose do not interrupt a running cell, so this is
safe with long computations; editing the code of a cell that is running
interrupts it and runs the new version.

For the final PDF, run `loom render book.typ` in a terminal (it compiles in
strict mode, which the preview must not use: do not pass
`--input loom=strict` to the preview task). The preview's Tinymist has its
own Typst compiler; loom needs Typst 0.15 or later, and the library reports
an older one in the preview. Check it with `tinymist compile` on a file
containing `#sys.version`.

## Caching and execution

A cell's stored result is reused only if its code, its execution options,
and every earlier cell of its session (including setup cells) are unchanged,
and if the packages loaded when it ran are unchanged. When a cell changes,
loom runs it and everything after it in the session. If the kernel's session
is not already in the state the changed cell expects, loom restarts the
session and runs all of its cells again, so the outputs of a session always
come from one uninterrupted execution. Appending cells at the end of a
chapter therefore runs only the new cells, while editing an earlier cell
reruns the chapter.

### Stateless cells

A cell marked `#| stateless: true` declares that it only displays things: it
creates and rebinds no variables. Editing it does not affect later cells, and
when it follows the last cell that changes the session (the usual place of a
figure being polished), saving reruns that cell alone. loom checks the
declaration after every run of the cell and reports an error if the cell
assigned a name. Changes made in place (`push!(v, 1)`, `l.append(1)`) are not
detected. In R, rebinding a name to an equal value is not detected either.

### Packages

After every cell, the kernel reports the packages loaded in its process, and
loom stores a fingerprint of each with the result: the Julia manifest entry
(and the source files of packages added with `Pkg.develop`), the
`DESCRIPTION` file of an R package, the `RECORD` file of a Python
distribution, or the source files of a local module. The output of
`<command> --version` is stored as well. A result whose packages or language
version changed is stale; loom reruns it in a new kernel process, since a
process cannot replace a package it has loaded. Adding a package that no
cell loads changes nothing.

Results live in `.loom/cache/entries.json` and figures in `.loom/figures/`.
`loom clean` removes results and figures the document no longer uses.

## Configuration

`loom.toml`, next to the root file; everything is optional. `loom init`
writes a commented template.

```toml
[execute]
fig-width = 7
fig-height = 5
fig-format = "svg"
fig-dpi = 300
error = false

[julia]
command = "julia"
args = ["--startup-file=no", "--threads=1,1", "--project=@."]

[r]
command = "Rscript"

[python]
command = "python3"     # or ".venv/bin/python"
dataframe = "pandas"    # or "polars"
```

Kernels run with the directory of the root file as working directory.

## Command line

| Command | Purpose |
|---|---|
| `loom init [DIR]` | write the support files and `loom.toml` |
| `loom run ROOT [--force]` | execute stale cells (all cells with `--force`) |
| `loom render ROOT [-- TYPST_ARGS]` | run, then `typst compile` in strict mode |
| `loom watch ROOT` | keep kernels running and re-run on save |
| `loom list ROOT` | sessions and the state of their cells |
| `loom clean ROOT` | remove unused results and figures |
| `loom kernels list\|stop` | show or stop running kernels |

Kernels are started by the `loom` command that needs them and exit when it
does. Their logs are in `.loom/kernels/`.
