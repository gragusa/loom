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

## Quick start

```sh
# Install Julia/R project dependencies (if any)
julia --project=. -e 'using Pkg; Pkg.instantiate()'

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

Loom is usable today for both Julia and R workflows, and the recent ergonomics pass improved the default authoring model substantially:

- `loom init` now writes a single-entrypoint [`loom.typ`] interface alongside the lower-level support files.
- The recommended Typst setup is now just `#import "loom.typ": *`.
- Inline plot cells are supported via `#jlplot(...)` and `#rplot(...)`, so the common case no longer requires a manual `savefig` call plus a separate `jlfig`/`rfig` render step.
- `loom run` auto-manages daemons for one-off runs and no longer leaves them running by default after the command exits.
- The legacy low-level API (`jlrun`, `rrun`, `jlfig`, `rfig`, explicit `jlpp_savefig` / `loom_savefig`) remains supported for advanced workflows.

Current recommendation:

- Use `loom.typ` plus `#jlplot` / `#rplot` for normal figures.
- Keep the manual save-and-render path for multi-figure cells, explicit artifact naming, or layouts that need tighter Typst control.

## Cross-references

All figure helpers (`jlplot`, `rplot`, `jlfig`, `rfig`, `jlmarginfig`, `rmarginfig`, `jlwidefig`, `rwidefig`) support a `label:` parameter. Pass a Typst label to make the figure referenceable:

```typst
#rplot(
  id: "fig-crime-police",
  label: <fig-crime-police>,
  caption: [Crime vs. police presence.],
  ```r
  ggplot(df, aes(x = police_rate, y = crime_rate)) + geom_point()
  ```
)

As shown in @fig-crime-police, ...
```

The low-level API (`jlrun`/`rrun` + `loom_savefig` + manual `#figure(image(...)) <label>`) remains available for advanced cases such as multi-figure cells or composite layouts.

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
                  writes _loom_cache/_loom_cache.typ
                  writes _loom_data.typ
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
r = "Rscript"            # Path to R binary (omit to disable R)
r_port = 2160            # HTTP port for R daemon

# Output
cache_dir = "_loom_cache"     # Directory for cached cell outputs
data_file = "_loom_data.typ"  # Generated Typst data file

# Default figure dimensions (inches)
# Applied when a chunk doesn't specify fig-width / fig-height
fig_width = 7
fig_height = 5
```

**Priority chain:** chunk option > `loom.toml` value > hardcoded default.

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

### `loom watch [ROOT]`

Watch `.typ` sources for changes and re-run only affected chapters. Designed for use alongside Tinymist live preview.

### `loom list [ROOT]`

Show chapters, cell counts, and languages used (no execution).

### `loom daemon list`

Show all running loom daemons (port and PID).

### `loom daemon stop [--port PORT] [--all]`

Stop daemons. Without flags, stops the daemon on the configured Julia port. Use `--all` to stop every running loom daemon.

## Cell syntax

### Julia

```typst
#import "loom.typ": *

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

// Inline plot cell: execute + capture + render in one place
#jlplot(id: "plot-id", caption: [A sine wave.], ```julia
using Plots
plot(0:0.1:2pi, sin.(0:0.1:2pi); label = "")
```)

// Table (captures MIME"text/typst" from SummaryTables.jl etc.)
#jltable(id: "tbl-id", caption: [Results.], ```julia
using SummaryTables
Table(df)
```)
```

### R

```typst
#import "loom.typ": *

// Silent execution (with optional message/warning filtering)
#rrun(id: "setup", message: false, warning: false, ```r
library(ggplot2)
df <- mtcars
```)

// Console with R> prompts
#rconsole(id: "demo", ```r
summary(df$mpg)
cor(df$mpg, df$wt)
```)

// Inline plot cell
#rplot(id: "plot-id", caption: [Scatter plot.], ```r
library(ggplot2)
ggplot(df, aes(wt, mpg)) + geom_point()
```)

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

Example:

```typst
#rrun(id: "wide-plot", fig-width: 10, fig-height: 4, ```r
p <- ggplot(df, aes(x, y)) + geom_point()
loom_savefig(p, "wide")
```)
```

### Saving figures

For the new default inline flow, prefer `#jlplot` / `#rplot` and return a plot object.

Manual save helpers are still available for advanced workflows such as multiple figures from one cell or explicit artifact naming. Inside `#jlrun` / `#rrun` cells, use:

**Julia:**
```julia
jlpp_savefig(plot_object, "name"; fmt=:svg, width=7, height=5)
```

**R:**
```r
loom_savefig(plot_object, "name", fmt = "svg", width = 7, height = 5)
```

Both helpers read default dimensions from `fig-width`/`fig-height` chunk options or `loom.toml` settings. You can override per-call.

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
  _loom_cache/         # Generated by loom
  _loom_data.typ       # Generated by loom
```

Each `#include` from the root file defines a chapter. A file named `preamble.typ` is special: its cells run first and are replayed into every chapter's session.

### Single-file document

Loom also works with a single `.typ` file — all cells belong to one chapter named after the file:

```
my-paper/
  paper.typ           # All cells inline
  loom.toml
  _loom_cache/
  _loom_data.typ
```

### Setting up imports

Preferred setup:

```typst
#import "loom.typ": *
```

Legacy low-level imports via `julia.typ`, `r.typ`, and `_loom_data.typ` still work.

## Session model

- Each chapter gets its own **Julia Module** and **R environment**.
- Variables from one chapter do not leak into another.
- The **preamble** session runs before all others; its code is replayed into fresh sessions after a reset.
- Julia and R cells can coexist in the same chapter — they run in separate per-language sessions.
- Use `session: "name"` to override the default session assignment.

## Caching

Loom caches cell results using content-addressable hashing (SHA-256 of the cell code). On subsequent runs, cells whose code hasn't changed are skipped. Use `--force` to re-execute everything.

Cache structure:

```
_loom_cache/
  preamble/
    manifest.json
    figures/
  intro/
    manifest.json
    figures/
      sincos.svg
      histogram.svg
  _loom_cache.typ      # Consolidated Typst data
```

## Live preview

Open two terminals:

```sh
# Terminal 1: watch and auto-rerun changed chapters
loom watch

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
