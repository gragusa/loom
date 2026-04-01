# Installing Loom

## Prerequisites

- **Rust** 1.85+ (to build from source)
- **Typst** 0.13+
- **Julia** 1.10+ (if using Julia cells)
- **R** 4.0+ (if using R cells)

## Build and install

```sh
cargo install --path .
```

This places the `loom` binary in `~/.cargo/bin/`. Make sure that directory is in your `PATH`.

The binary is self-contained: the Julia and R daemon scripts are embedded and extracted to a temporary directory at runtime.

## Language setup

### Julia

Loom requires the `JSON3` package in the active Julia project. On the first run, if `JSON3` is not found, Loom will offer to create a local environment and add it automatically.

To set it up manually:

```sh
julia --project=. -e 'using Pkg; Pkg.add("JSON3")'
```

### R

Install the required R packages once:

```r
install.packages(c("jsonlite", "httpuv"))
```

## Quick start

```sh
# Create a new project directory
mkdir my-project && cd my-project

# Initialize loom (creates .loom/ directory and loom.toml)
loom init

# Write a Typst file
cat > hello.typ << 'EOF'
#import ".loom/loom.typ": *

#jlconsole(id: "hello", ```julia
println("Hello from Julia!")
1 + 1
```)
EOF

# Execute all code cells
loom run hello.typ

# Compile the PDF
typst compile hello.typ
```

## What `loom init` creates

```
my-project/
  loom.toml          # Configuration (edit this)
  .loom/             # Loom-managed files (do not edit)
    loom.typ         # Single-entrypoint API
    julia.typ        # Julia rendering functions
    r.typ            # R rendering functions
    _loom_data.typ   # Placeholder data file
```

After `loom run`, the `.loom/` directory also contains:

```
  .loom/
    _loom_style.typ        # Style settings from loom.toml
    _loom_cache/           # Cached cell outputs
      _loom_cache.typ      # Consolidated Typst data
      <chapter>/
        manifest.json
        figures/
```

## Sharing a project

To share a project with someone who does **not** have Loom installed, include the `.loom/` directory. It contains all pre-computed results — Typst can compile the document directly without running any code.

To share with someone who **does** have Loom, they only need:
- Your `.typ` source files
- `loom.toml`
- Any Julia/R project files (`Project.toml`, `renv.lock`, etc.)

They can then run `loom run` to regenerate the `.loom/` directory.

## Verifying the installation

```sh
# Check that loom is in your PATH
loom --version

# Check that Typst is available
typst --version

# Check Julia (if using Julia cells)
julia --version

# Check R (if using R cells)
Rscript --version
```
