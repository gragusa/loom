= Getting started

loom runs Julia, R, and Python code written inside a Typst document and lets
Typst typeset the results. A code cell is an ordinary Typst code block whose
language tag is `julia`, `r`, or `python`; options for the cell go in
comment lines at its top, as in Quarto. loom executes the cells in
persistent processes called kernels, stores the results in the `.loom/`
directory next to the document, and a small Typst library turns those
results into code listings, text output, figures, and tables.

Typst never runs code. The document therefore compiles at any time, with or
without loom, and Tinymist's live preview keeps working while you write:
each time you save, loom executes what changed and the preview picks up the
new results.

This manual is a loom document. Every result in it was computed when the
manual was built.

== Installation

loom is a single program written in Rust. From a copy of its repository:

```sh
cargo install --path .
```

The program contains the three kernels and the Typst library, and writes
them into a project when they are needed. The languages themselves are
needed only if a document uses them:

- Typst 0.15 or later. loom finds cells with the parser of Typst 0.15, and
  the Typst library refuses older versions.
- Julia 1.10 or later. The Julia kernel uses only the standard library, so
  nothing has to be added to your environments.
- R 4.0 or later with the packages `jsonlite` and `evaluate`. When `svglite`
  is installed, R figures are written with it.
- Python 3.9 or later. Figures need matplotlib.

== A first document

Create a directory, write the support files, and start a document:

```sh
mkdir paper && cd paper
loom init
```

`loom init` writes `.loom/loom.typ` (the Typst library), an empty
`.loom/cells.json` (the results), and a commented `loom.toml`. The document
enables loom with two lines and then uses code blocks as cells:

````typ
#import ".loom/loom.typ": loom
#show: loom

= Results

```julia
x = [1.0, 2.0, 4.0]
sum(x) / length(x)
```

The mean is `{julia} sum(x) / length(x)`.
````

Run the cells and compile:

```sh
loom run paper.typ
typst compile paper.typ
```

The cell above, run by this manual, gives:

```julia
x = [1.0, 2.0, 4.0]
sum(x) / length(x)
```

and the sentence reads: the mean is `{julia} sum(x) / length(x)`.

== The commands you use most

`loom run FILE` executes every cell whose stored result is out of date and
updates `.loom/cells.json`. Cells that did not change are not executed
again.

`loom watch FILE` does the same each time a source file is saved, keeping
the kernels running in between. Open the document in an editor with the
Tinymist extension in a second window; the preview updates after every save.

`loom render FILE` runs the cells and then calls `typst compile` in strict
mode, in which any cell without a current result stops the compilation. Use
it for the final PDF. Arguments after `--` are passed to Typst:

```sh
loom render paper.typ -- paper-final.pdf
```

The remaining commands are described in @sec-cli.

== How it works

loom reads the document with the Typst parser and finds the cells, following
`#include` directives. It groups cells into sessions (by default one per
chapter and language), computes a key for every cell that depends on its
code, its options, and all earlier cells of its session, and executes the
cells whose key has no stored result. Results are stored under their key,
so an edit never overwrites a result: it produces a new key.

After a run, loom writes `.loom/cells.json`, which maps the text of each
cell to its results. The Typst library looks results up by the text of the
code block it is showing. A block whose text has no result (because you are
editing it) is shown as not yet executed; see @sec-preview.
