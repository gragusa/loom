= Caching and execution

== When a cell runs again

loom reuses a stored result only when the cell would compute the same thing
again. Concretely, a result is reused when all of these are unchanged:

- the cell's code and its execution options;
- every earlier cell of its session, including the setup cells of its
  language, together with their options;
- the packages that were loaded when the cell ran, and the version of the
  language (@sec-packages).

Display options, prose, and cells of other sessions do not matter. When a
cell must run, every later cell of its session runs too, because it may
depend on what changed.

== Resuming and rebuilding sessions

A kernel keeps its sessions between runs of `loom watch`. When the first
cell to run follows directly on what the kernel has already executed, loom
continues the session from there: adding cells at the end of a chapter runs
only the new cells. Otherwise, for example after an edit to an earlier cell,
loom discards the session and runs all its cells again from the start. A
session's outputs therefore always come from one uninterrupted run, even
when code draws random numbers without a seed.

`loom run` starts new kernels, so it always rebuilds the sessions that have
something to run.

== Stateless cells <sec-stateless>

Most edits to a finished chapter concern its figures: colors, labels,
sizes. A cell that only displays something can say so with
`stateless: true`. Such a cell is left out of the chain of keys, so editing
it does not make later cells stale, and running it does not change the
session's state. When it comes after the last cell that changes the
session, which is where the figure being polished usually is, saving runs
that cell alone.

```julia
using Plots
xs = range(0, 4π; length = 400);
```

```julia
#| stateless: true
#| label: fig-damped
#| fig-cap: A stateless figure cell: editing it reruns only this cell.
plot(xs, exp.(-xs ./ 4) .* sin.(xs); label = "", lw = 2)
```

loom checks the claim every time the cell runs: if a stateless cell creates
or rebinds a variable, the run fails and names the variable. Changes made
in place, such as `push!(v, 1)` in Julia or `l.append(1)` in Python, cannot
be detected; in R, rebinding a name to an equal value is not detected
either. Marking a cell stateless is your statement that it does neither.

== Packages and language versions <sec-packages>

After every cell, the kernel reports the packages loaded in its process,
and loom stores with the result a fingerprint of each:

- Julia: the package's entry in the manifest of the environment that
  provides it, and the source files of packages added with `Pkg.develop`;
- R: the `DESCRIPTION` file of the installed package;
- Python: the `RECORD` file of the installed distribution, or the source
  files of an editable install or of a module in the project directory.

The output of `julia --version` (or `Rscript --version`, or
`python3 --version`, with the arguments of `loom.toml`) is stored as well. Before
reusing a result, loom recomputes these fingerprints. A result whose
packages or language changed is stale and runs again in a new kernel
process, since a running process cannot replace a package it has loaded.
Adding a package that no cell loads changes nothing.

This also covers your own code: a module or a package under development
that a cell loads is part of that cell's environment, and editing it makes
the cells that loaded it run again.

== Files

/ `.loom/loom.typ`: The Typst library. Every loom command rewrites it, so that it always
  matches the program.

/ `.loom/cells.json`: The results of the current document, read by the library. Written
  atomically, so that Tinymist never reads half a file.

/ `.loom/figures/`: Images, named by the hash of their contents. An updated figure is a new
  file, so a preview never shows an outdated image.

/ `.loom/cache/entries.json`: Every stored result, by key, with its package environment. Results of
  code that no longer appears in the document are kept (up to 200), so that
  undoing an edit finds the earlier result.

/ `.loom/kernels/`: Kernel scripts, logs, and runtime files.

`loom clean FILE` removes stored results and figures that the document no
longer uses. Deleting `.loom/` altogether is always safe; the next run
recomputes everything.

To share a document with someone who does not have loom, include `.loom/`
(without `kernels/`): Typst needs only `loom.typ`, `cells.json`, and the
figures.
