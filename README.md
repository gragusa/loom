# Loom

A code preprocessor for Typst that weaves Julia and R output into documents. Write code directly in `.typ` source files, and loom executes it through persistent daemons, caches the results, and generates a data file your document imports. Typst never runs code — it just typesets pre-computed results.

## Install

```sh
cargo install --path .
```

The binary is self-contained: both daemon scripts (Julia and R) are embedded and extracted to a temp directory at runtime.

## Prerequisites

- **Typst** 0.13+
- **Julia** 1.10+ (if using Julia cells)
- **R** 4.0+ with `jsonlite` and `httpuv` packages (if using R cells)
- **Rust** 1.85+ (to build from source)

For R, install the required packages once:

```r
install.packages(c("jsonlite", "httpuv"))
```

For Julia, Loom needs `JSON3` in the active project. If `loom run` or `loom watch`
does not find it, Loom will prompt to either create a local environment and add
`JSON3`, or add `JSON3` to the existing environment in the current directory.

## Quick start

```sh
# Initialize support files once
loom init

# Execute all code cells (starts daemons automatically)
loom run book.typ

# Compile the PDF
typst compile book.typ

# For live preview, keep daemons around only while watching
loom watch book.typ
```

## Status

Loom provides a complete authoring workflow for both Julia and R in Typst documents:

- **Single import**: `#import ".loom/loom.typ": *` is all you need — no manual imports of `julia.typ`, `r.typ`, or `_loom_data.typ`.
- **Inline plots**: `#jlplot(...)` and `#rplot(...)` execute code, capture the figure, and render it in one call.
- **Cross-references**: all figure helpers accept `label:` so figures are referenceable via `@label` without manual Typst wrappers.
- **Placement control**: `place: "body"` (default), `"margin"`, or `"wide"` on plot cells.
- **Daemon lifecycle**: `loom run` auto-starts and auto-stops daemons. By default, Loom starts only the runtimes used by the current document; set `prestart_all_languages = true` to keep both configured daemons warm. Idle daemons shut down after 30 minutes (configurable).
- **Caching**: content-addressable hashing skips unchanged cells on subsequent runs.
- **Fail-fast execution**: transport failures, reset failures, and cell errors now stop `loom run` instead of silently reusing stale cache output.

The legacy low-level API (`jlrun`, `rrun`, `jlfig`, `rfig`, explicit `jlpp_savefig` / `loom_savefig`) remains supported for advanced workflows like multi-figure cells or composite layouts.

## Author API

All functions are available after `#import ".loom/loom.typ": *`.

| Function | Purpose |
|----------|---------|
| `#jlrun(id, ...)` | Silent Julia execution (no output in document) |
| `#rrun(id, ...)` | Silent R execution |
| `#jlconsole(id, ...)` | Julia code with `julia>` prompts and interleaved output |
| `#rconsole(id, ...)` | R code with `r>` prompts and interleaved output |
| `#jlplot(id, ...)` | Julia figure: execute + capture + render |
| `#rplot(id, ...)` | R figure: execute + capture + render |
| `#jltable(id, ...)` | Julia table (SummaryTables.jl etc.) |
| `#rtable(id, ...)` | R table (tinytable, gt) |

### Plot cell parameters

```typst
#rplot(
  id: "fig-scatter",          // required: cell identifier
  caption: [A scatter plot.],  // optional: figure caption
  label: <fig-scatter>,        // optional: Typst label for @fig-scatter
  place: "body",               // optional: "body" (default), "margin", or "wide"
  width: 80%,                  // optional: image width (body placement only)
  dy: 0pt,                     // optional: vertical caption offset
  fig-width: 7,                // optional: R/Julia figure width in inches
  fig-height: 5,               // optional: R/Julia figure height in inches
  ```r
  library(ggplot2)
  ggplot(mtcars, aes(wt, mpg)) + geom_point()
  ```
)
```

The low-level figure helpers (`jlfig`, `rfig`, `jlmarginfig`, `rmarginfig`, `jlwidefig`, `rwidefig`) accept the same `label:`, `caption:`, `dy:`, and `width:` parameters.

Console prompt text and size can be configured in `loom.toml` under `[style]`. The Typst setters (`#jl-set-prompt`, `#r-set-prompt`, `#jl-set-prompt-size`, `#r-set-prompt-size`) remain available for manual overrides in the document preamble.

## Cross-references

Pass `label:` to any figure helper to make it referenceable:

```typst
#rplot(
  id: "fig-crime",
  label: <fig-crime>,
  caption: [Crime vs. police presence per 100k inhabitants.],
  width: 72%,
  ```r
  ggplot(crime_plot, aes(x = police_rate, y = crime_rate)) +
    geom_point(color = "darkblue", size = 2.5)
  ```
)

As shown in @fig-crime, the correlation is positive but misleading.
```

This works for all placement modes (`"body"`, `"margin"`, `"wide"`) and both languages. No manual `#figure(image(...)) <label>` wrapper is needed.

## How it works

```
  .typ sources ──> loom (Rust) ──> Julia daemon (TCP, port 2159)
                       |           R daemon (HTTP, port 2160)
                       |                  |
                       |           executes cells,
                       |           captures stdout/stderr,
                       |           saves figures
                       |                  |
                       <------------------'
                       |
                  writes .loom/_loom_cache/_loom_cache.typ
                  writes .loom/_loom_data.typ
                       |
                       v
                  typst compile ──> book.pdf
```

Loom is language-agnostic at the protocol level. Julia uses a TCP daemon with newline-delimited JSON; R uses an HTTP daemon (httpuv) with JSON POST requests. Both speak the same logical protocol.

## Configuration

Create a `loom.toml` in your project root. All fields are optional:

```toml
# Language runtimes
julia = "julia"          # Path to Julia binary
julia_port = 2159        # TCP port for Julia daemon
r = "Rscript"            # Path to R binary
r_port = 2160            # HTTP port for R daemon
prestart_all_languages = false  # Start both configured daemons even before cells appear

# Output
cache_dir = ".loom/_loom_cache"     # Directory for cached cell outputs
data_file = ".loom/_loom_data.typ"  # Generated Typst data file

# Default figure dimensions (inches)
fig_width = 7
fig_height = 5

# Daemon idle timeout in seconds (0 = no timeout)
idle_timeout = 1800           # Default: 30 minutes

[style]
jl_code_size = "10pt"                 # Julia console code font size
jl_prompt_size = "10pt"               # Julia console prompt font size
jl_prompt_text = "\"julia> \""        # Julia console prompt text
r_code_size = "10pt"                  # R console code font size
r_prompt_size = "10pt"                # R console prompt font size
r_prompt_text = "\"r> \""             # R console prompt text
output_color = "luma(100)"            # Console output text color
block_fill = "luma(248)"              # Console block background
block_inset = "8pt"                   # Console block padding
block_radius = "2pt"                  # Console block corner radius
block_stroke = "0.5pt + luma(220)"    # Console block border
line_spacing = "0.55em"               # Line spacing in console blocks
font = "DejaVu Sans Mono"            # Monospace font for all console blocks
caption_size = "12pt"                 # Caption font size
caption_dy = "1.75em"                 # Caption vertical offset (margin mode)
caption_gap = "1em"                   # Gap between figure and caption (inline mode)
```

All `[style]` values are raw Typst expressions passed directly to rendering functions.

**Priority chain:** CLI flag > chunk option > `loom.toml` value > hardcoded default.

## CLI reference

### `loom run [ROOT]`

Execute code cells and generate output files.

| Flag | Default | Description |
|------|---------|-------------|
| `ROOT` | `book.typ` | Root Typst file |
| `-c, --chapter NAME` | all | Re-run only this chapter |
| `--cache-dir DIR` | from config | Cache directory |
| `-f, --force` | off | Ignore cache, re-execute everything |
| `--port PORT` | from config | Julia daemon TCP port |
| `--idle-timeout SECS` | 1800 | Daemon idle timeout (0 to disable) |

### `loom watch [ROOT]`

Watch `.typ` sources for changes and re-run only affected chapters. Designed for use alongside Tinymist live preview. Accepts the same `--cache-dir`, `--port`, and `--idle-timeout` flags.

### `loom list [ROOT]`

Show chapters, cell counts, and languages used (no execution).

### `loom daemon list`

Show all running loom daemons (port and PID).

### `loom daemon stop [--port PORT] [--all]`

Stop daemons. Without flags, stops the daemon on the configured Julia port. Use `--all` to stop every running loom daemon.

## Cell syntax

### Julia

```typst
#import ".loom/loom.typ": *

// Silent execution (no visible output)
#jlrun(id: "setup", ```julia
using Statistics
x = randn(100)
```)

// Console with julia> prompts and interleaved output
#jlconsole(id: "demo", ```julia
mean(x)
std(x)
```)

// Inline plot — execute, capture, and render in one call
#jlplot(id: "fig-hist",
  caption: [Distribution of 1000 draws from N(0,1).],
  label: <fig-hist>,
  place: "margin",
  ```julia
  using Plots
  histogram(randn(1000); bins = 30, label = "")
  ```)

See @fig-hist for the result.

// Table (captures MIME"text/typst" from SummaryTables.jl etc.)
#jltable(id: "tbl-id", caption: [Results.], ```julia
using SummaryTables
Table(df)
```)
```

### R

```typst
#import ".loom/loom.typ": *

// Silent execution (with optional message/warning filtering)
#rrun(id: "setup", message: false, warning: false, ```r
library(ggplot2)
df <- mtcars
```)

// Console with r> prompts
#rconsole(id: "demo", ```r
summary(df$mpg)
cor(df$mpg, df$wt)
```)

// Inline plot with cross-reference
#rplot(id: "fig-scatter",
  caption: [Weight vs. fuel economy.],
  label: <fig-scatter>,
  width: 80%,
  ```r
  ggplot(df, aes(wt, mpg)) +
    geom_point() +
    labs(x = "Weight (1000 lbs)", y = "Miles per gallon")
  ```)

@fig-scatter shows a clear negative relationship.

// Table (tinytable or gt)
#rtable(id: "tbl-id", caption: [Coefficients.], ```r
library(tinytable)
tt(coef_df)
```)
```

### Chunk options

Parsed from the cell header. These control the daemon's behavior:

| Option | Default | Description |
|--------|---------|-------------|
| `message` | `true` | Include R `message()` output |
| `warning` | `true` | Include R `warning()` output |
| `error` | `true` | Include errors in output |
| `echo` | `true` | Show source code |
| `eval` | `true` | Execute the code |
| `results` | `"markup"` | Output mode: `markup`, `asis`, `hold`, `hide` |
| `fig-width` | from config | Figure width in inches |
| `fig-height` | from config | Figure height in inches |
| `comment` | `"##"` | Prefix for R output lines |
| `collapse` | `false` | Merge code and output |

### Saving figures (advanced)

For the default flow, prefer `#jlplot` / `#rplot` — just return the plot object from the code block.

Manual save helpers are available for advanced workflows such as multiple figures from one cell or explicit artifact naming. Inside `#jlrun` / `#rrun` cells:

**Julia:**
```julia
jlpp_savefig(plot_object, "name"; fmt=:svg, width=7, height=5)
```

**R:**
```r
loom_savefig(plot_object, "name", fmt = "svg", width = 7, height = 5)
```

Both helpers read default dimensions from `fig-width`/`fig-height` chunk options or `loom.toml` settings.

## Document structure

### Multi-file book

```
my-book/
  book.typ            # Root: imports + includes
  loom.toml           # Configuration
  preamble.typ        # Shared session (loaded into all chapters)
  chapter/
    intro.typ          # Chapter "intro"
    analysis.typ       # Chapter "analysis"
  .loom/               # Generated by loom
    loom.typ
    julia.typ
    r.typ
    _loom_data.typ
    _loom_style.typ
    _loom_cache/
```

Each `#include` from the root file defines a chapter. A file named `preamble.typ` is special: its cells run first and are replayed into every chapter's session.

### Single-file document

Loom also works with a single `.typ` file — all cells belong to one chapter named after the file:

```
my-paper/
  paper.typ           # All cells inline
  loom.toml
  .loom/              # Generated by loom
```

### Setting up imports

```typst
#import ".loom/loom.typ": *
```

This is the only import you need. Legacy low-level imports via `.loom/julia.typ`, `.loom/r.typ`, and `.loom/_loom_data.typ` still work.

## Session model

- Each chapter gets its own **Julia Module** and **R environment**.
- Variables from one chapter do not leak into another.
- The **preamble** session runs before all others; its code is replayed into fresh sessions after a reset.
- Julia and R cells can coexist in the same chapter — they run in separate per-language sessions.
- Use `session: "name"` to override the default session assignment.
- Shared `session:` names are rebuilt from preamble plus earlier chapters that use the same session, in document order, whenever a chapter is re-run.

## Caching

Loom caches cell results using content-addressable hashing (SHA-256 of the cell code). On subsequent runs, cells whose code hasn't changed are skipped. Use `--force` to re-execute everything.

If a run fails, Loom now aborts immediately and leaves the previous cache/data files in place rather than writing a partially updated result set.

Cache structure:

```
.loom/
  _loom_cache/
    preamble/
      manifest.json
      figures/
    intro/
      manifest.json
      figures/
        sincos.svg
        histogram.svg
    _loom_cache.typ    # Consolidated Typst data
```

## Daemon lifecycle

Daemons are spawned automatically by `loom run` and `loom watch`. Their lifecycle:

- **`loom run`**: spawns the runtimes the document currently needs, executes all cells, then stops them on exit.
- **`loom watch`**: starts the runtimes the document currently needs and keeps them alive while watching. If you add the other language later, Loom starts that daemon on demand. Set `prestart_all_languages = true` to start both configured runtimes immediately.
- **Idle timeout**: daemons self-terminate after 30 minutes of inactivity (configurable via `idle_timeout` in `loom.toml` or `--idle-timeout` on the CLI). Set to `0` to disable.
- **Manual control**: `loom daemon list` shows running daemons; `loom daemon stop --all` stops them.

## Live preview

Open two terminals:

```sh
# Terminal 1: watch and auto-rerun changed chapters
loom watch book.typ

# Terminal 2: VS Code with Tinymist extension
# Preview refreshes automatically when loom rewrites the cache
```

## Table support

### Julia

Use any package that implements `show(io, MIME"text/typst", obj)`:

- **SummaryTables.jl** — `Table(...)` objects render directly

### R

- **tinytable** — `tt()` objects saved via `save_tt()` to `.typ` format
- **gt** (>= 0.12) — `gt()` objects exported via `as_typst()`

Tables are captured automatically when using `#jltable` / `#rtable`.
