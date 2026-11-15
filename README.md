# loom

loom executes Julia, R, and Python in Typst documents. Code cells are ordinary 
Typst code blocks with Quarto-style options

## Requirements:

- Typst 0.15. loom locates cells with the parser of Typst 0.15, and the Typst
  library must agree with it on the text of each code block.
- Julia 1.10 or later, for Julia cells. No packages are required in your
  environment.
- R 4.0 or later with `jsonlite` and `evaluate`, for R cells. `svglite` is
  used for SVG figures when installed.
- Python 3.9 or later, for Python cells. Figures need matplotlib.


## Install

```sh
cargo install --path .
```

## Documentation

The manual which is itself a loom document cab be built by:

```sh
cd docs/manual/
julia --project=. -e 'using Pkg; Pkg.instantiate()'
Rscript -e 'renv::restore()'
python3 -m venv .venv && .venv/bin/pip install -r requirements.txt
loom render manual.typ -- loom-manual.pdf
```



## Quick start

```sh
loom init                 # writes .loom/loom.typ, .loom/cells.json, loom.toml
loom run paper.typ        # executes the cells
typst compile paper.typ   #
```

For live preview, run `loom watch paper.typ` in a terminal and open the file
in an editor with Tinymist. Each time you save, loom executes what changed and
the preview updates.

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

## Zed with the typst-live extension

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
