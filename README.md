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
| `out-width` | | `fig-width` | displayed width, e.g. `80%` or `3in` |
| `column` | | `body` | `margin` or `page`; see below |

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

## Books and sessions

Each `#include` in the root file starts a chapter, named after the included
file. Every chapter gets its own session per language, so variables do not
leak between chapters. Cells with the same `session:` name share a session
across chapters, in document order. Setup cells (`#| setup: true`, or any cell
in a file named `preamble.typ`) run first and are replayed at the start of
every session of their language.

## Live preview

Tinymist recompiles on every keystroke, before you save; loom only sees saved
files. The Typst library therefore marks each cell:

- a cell whose code (or an *exec* option) differs from the stored result is
  shown as **stale**, with its old output;
- a new cell is shown as **not executed yet**;
- a cell whose code raised an error shows the error.

Saving updates the results. `loom render` (or `typst compile --input
loom=strict`) refuses to compile while any cell is in one of these states.

## Caching and execution

A cell's stored result is reused only if its code, its execution options, and
every earlier cell of its session (including setup cells) are unchanged. When
a cell changes, loom runs it and everything after it in the session. If the
kernel's session is not already in the state the changed cell expects, loom
restarts the session and runs all of its cells again, so the outputs of a
session always come from one uninterrupted execution. Appending cells at the
end of a chapter therefore runs only the new cells, while editing an earlier
cell reruns the chapter.

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
command = "python3"
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
