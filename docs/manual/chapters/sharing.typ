= Sharing data between languages <sec-sharing>

Each language runs in its own kernel, so a variable defined in R does not
exist in Python or Julia. Data moves between them through two cell options:
`export` names values that a cell makes available after it runs, and
`import` names values that a cell needs before it runs. loom carries the
values between kernels as files in a format all three languages read.

== An example

R prepares a data frame:

```r
#| export: cars
cars <- mtcars[, c("mpg", "wt", "hp", "cyl")]
cars$cyl <- factor(cars$cyl)
```

Python summarizes it:

```python
#| import: cars
cars.groupby("cyl", observed=True)["mpg"].mean().round(2)
```

Julia fits a regression and exports the coefficients:

```julia
#| import: cars
#| export: beta
using DataFrames
X = hcat(ones(nrow(cars)), cars.wt, cars.hp)
beta = X \ cars.mpg
```

and R reads them back:

```r
#| import: beta
names(beta) <- c("intercept", "wt", "hp")
round(beta, 4)
```

An import reads the value of the latest earlier cell, in document order,
that exports the name, whatever its language or session. The same options
therefore also share data between chapters of one language without merging
their sessions. Importing a name that no earlier cell exports, or exporting
a name the cell did not define, is an error that names the cell.

== What can be shared

Data frames, arrays, and single values cross between languages; objects
with a class of their own do not. Exporting a fitted model, an S4 object, a
Julia struct, or a Python object is an error that lists what can be shared.

#figure(
  table(
    columns: (auto, 1fr, 1fr, 1fr),
    align: left,
    stroke: none,
    table.hline(),
    table.header([*Kind*], [*R*], [*Python*], [*Julia*]),
    table.hline(stroke: 0.5pt),
    [data frame], [`data.frame` (also tibble, data.table)], [pandas `DataFrame` (or polars)], [any table; arrives as `DataFrame`],
    [array], [vector, `matrix`, `array`], [`numpy.ndarray`], [`Array`],
    [single value], [length-1 vector], [`int`, `float`, `bool`, `str`], [number, `Bool`, `String`],
    table.hline(),
  ),
  caption: [Values that can be shared, and what they become.],
) <tbl-shared>

Data frame columns may hold numbers, integers, logicals, strings, factors
(categoricals), dates, and date-times, with missing values. R row names and
pandas indexes are not carried. Arrays hold numbers, logicals, or strings,
in any number of dimensions; the shape is kept, so a 3 by 4 matrix is a 3 by
4 matrix in every language. Arrays cannot contain missing values. R has no
single values, so a vector of length one is shared as a single value, as
reticulate does: `n <- nrow(df)` in R arrives as an integer in Julia and in
Python.

Integer widths are kept: an R integer is 32 bits and arrives in Julia as
`Int32`. Factors and categoricals arrive in R as factors, in Python as
pandas categoricals, and in Julia as strings. A factor exported from R
arrives everywhere as strings, because the R package that writes the files
(nanoarrow) cannot yet write categorical columns.

== Packages and environments

Values travel as Arrow files. Each language needs one package in the
environment of the document, loaded only when a cell imports or exports:

- R: `nanoarrow`;
- Python: `pyarrow`, and pandas or polars for data frames;
- Julia: `Arrow.jl`, and `DataFrames.jl` to receive data frames.

A missing package fails the cell with an error that names it. A document
that shares data is best given its own environment for each language,
next to the root file:

- Julia: a `Project.toml`, found through the default `--project=@.`;
- R: an renv project (`renv::init()`), activated by the `.Rprofile` that
  renv writes, since the kernel starts in the document's directory;
- Python: a virtual environment, named in `loom.toml`:

```toml
[python]
command = ".venv/bin/python"   # relative to the document's directory
dataframe = "pandas"           # or "polars"
```

This manual is built that way: `docs/manual/` holds `Project.toml`,
`renv.lock`, and `requirements.txt`.

== Order of execution

Each language runs on its own thread: while R computes, Python and Julia
can compute too. Within a language, cells run in document order. A cell
that imports waits until the cell exporting its data has finished. Because
an export always comes earlier in the document than the cells importing it,
these waits always end.

An importing cell's result depends on the exact result it imported: editing
the R cell above reruns the Python and Julia cells that read `cars`, and the
R cell that reads `beta`, and nothing else. In the preview, they are marked
stale until then.

Exported files are stored with the result of the cell that wrote them, so
an importing cell can run again without rerunning its exporter. If an
exporter is rerun (for example when its session is rebuilt) and produces
different data although its code did not change, loom warns: the cells that
imported the earlier data were not rerun. Code that draws random numbers
should set a seed.
