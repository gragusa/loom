# Loom kernel protocol, version 1

A kernel is a long-running process that executes code cells for one language
(Julia, R, or Python) on behalf of `loom`. Each kernel script is embedded in
the `loom` binary and written to `.loom/kernels/` before it is started. This
document is the contract between `loom` (the client) and the three kernels.

## Process

`loom` starts the kernel with the project's document directory as working
directory, stdout redirected to `/dev/null`, stderr appended to
`.loom/kernels/<language>.log`, and these arguments:

```
--connect <P>       TCP port on 127.0.0.1 where loom is listening
--token <T>         shared secret
--parent-pid <PID>  the kernel exits when this process no longer exists
--scratch <DIR>     directory where the kernel writes image files
```

The kernel opens no listening socket. `loom` listens on 127.0.0.1 before
starting the kernel; the kernel connects to `127.0.0.1:<P>` and, before
anything else, sends one line identifying itself:

```json
{"token": "T"}
```

`loom` closes the connection if the token does not match. Then `loom` sends
requests on that connection and the kernel answers them, for the lifetime of
the kernel. When the connection is closed (end of file, or a read or write
error), the kernel exits. It never reconnects, and it never exits because the
connection is idle.

The kernel also checks every two seconds whether the parent process is still
alive, including while user code is running when the language allows it, and
exits when it is not.

`loom` interrupts a running request by sending the kernel process SIGINT.
The request then finishes like a failed one: `failed: true`, with a last
`error` output whose text starts with `interrupted`, and the session's
position becomes `id`. A SIGINT that arrives while no request is running,
or while the kernel is reading a request or writing a response, is ignored:
the kernel keeps serving the connection.

## Framing

Each request and each response is one JSON object encoded as UTF-8 on a
single line terminated by `\n`. Every request receives exactly one response.
Every request carries the token; a request whose `token` does not match
receives `{"ok": false, "error": "invalid token"}` and the kernel exits.

`ok: false` signals a protocol-level problem (malformed request, unknown
`op`, position mismatch). It is never used for errors raised by user code.

## Sessions and positions

A session is a named namespace (a Julia `Module`, an R environment whose
parent is the global environment, a Python dict used as globals). Sessions
are independent of each other. Loaded packages are process-wide.

Each session has a *position*: the `id` of the last request executed in it,
or `null`. `loom` uses positions to verify that a session is in the state it
expects before it sends the next cell.

## Requests

### `hello`

```json
{"op": "hello", "token": "T"}
```

```json
{"ok": true, "protocol": 1, "language": "julia", "version": "1.13.3", "pid": 4242}
```

`pid` is the id of the process that executes code. It can differ from the
process `loom` started, when the command is a launcher (such as juliaup's
`julia`) that runs the language in a child process; `loom` sends SIGINT to
`pid`.

### `reset`

```json
{"op": "reset", "token": "T", "session": "intro", "position": "abc123",
 "setup": ["using Statistics", "f(x) = 2x"]}
```

Discards the session if it exists and creates an empty one. Each string in
`setup` is then executed in order in that session, without capturing output
(anything printed goes to the process's stdout or stderr) and without
producing images. On success the session's position becomes `position`:

```json
{"ok": true, "failed": false, "outputs": []}
```

If setup string `i` raises, the remaining strings are skipped, the position
becomes `null`, and the response carries one `error` output:

```json
{"ok": true, "failed": true, "outputs": [{"type": "error", "text": "..."}]}
```

### `exec`

```json
{"op": "exec", "token": "T", "session": "intro", "after": "abc123",
 "id": "def456", "mode": "block", "code": "x = 1\nx + 1",
 "options": {"fig-width": 7.0, "fig-height": 5.0, "fig-format": "svg", "fig-dpi": 300, "print-width": 80}}
```

- If the session does not exist, or its position is not equal to `after`,
  the kernel executes nothing and answers
  `{"ok": false, "error": "position mismatch: session 'intro' is at <position>, request expects <after>"}`.
- Otherwise it executes `code` in the session and answers

  ```json
  {"ok": true, "failed": false, "outputs": [ ... ],
   "changed": ["x", "f"], "packages": [ ... ]}
  ```

  and the session's position becomes `id`, whether or not the user code
  raised. `changed` and `packages` are described under "State and
  packages" below. `failed` is `true` when the code raised an error; execution of the
  cell stops at that error, and the last output is an `error` output.

`options` always contains these five keys; for Python it may also contain
`dataframe` (`"pandas"` or `"polars"`, see "Shared data"). `fig-width` and `fig-height` are in
inches. `fig-format` is one of `svg`, `png`, `pdf`; a kernel produces a
figure in that format when the plotting package can. `fig-dpi` applies to
raster output. `print-width` (an integer, at least 20) is the number of
characters per line of printed output: Julia uses it as the display width of
values and sets `ENV["COLUMNS"]`; R sets `options(width =)`; Python sets
`COLUMNS` and, when they are loaded, numpy's `linewidth` and pandas'
`display.width`.

`mode` is `block` or `inline`; see below.

Optional fields `imports` and `exports` move data between sessions and
languages; see "Shared data" below.

Optional fields `file` (string) and `line` (integer) give the source file of
the cell and the line of the first line of `code` in it. A kernel should use
them so that error messages and tracebacks refer to the document, for
example `chapter/intro.typ:42`; a kernel that cannot do so ignores them.

### `shutdown`

```json
{"op": "shutdown", "token": "T"}
```

Answers `{"ok": true}` and exits.

## State and packages

Every `exec` response, in both modes and whether or not the code failed,
carries two more fields.

`changed` lists, sorted, the names in the session namespace that the request
created or rebound: names present after the request that were absent before,
or whose binding now refers to a different object. Rebinding is detected by
object identity where the language provides one (Julia `===`, Python `is`,
R environments and external pointers): `x = x` changes nothing, while
`x = copy(x)` changes `x`. R compares other objects by value, so in R
rebinding a name to an equal copy is not reported; mutating an object in place (`push!(v, 1)`, `v[1] <- 0` on an
environment, `l.append(1)`) is not reported. Names the kernel or the language
creates on its own (for example Julia's `ans`, R's `.Random.seed`, Python's
`__builtins__` and names starting with `__`) are excluded. Names removed by
the request are included.

`packages` lists every package loaded in the process when the request ends,
except those that ship with the language itself (Julia standard libraries, R
base and recommended packages with `Priority: base`, Python standard library
modules). Each entry is

```json
{"name": "Plots", "version": "1.41.6", "kind": "manifest", "path": "/abs/Manifest.toml", "uuid": "91a5bcdd-..."}
```

with `kind` one of:

- `manifest` (Julia): `path` is the manifest file of the environment that
  provides the package and `uuid` its UUID;
- `file`: `path` is a file whose contents change whenever the installed
  package changes: R `<library>/<package>/DESCRIPTION`; Python the `RECORD`
  file of the distribution's `.dist-info` directory;
- `tree`: `path` is a directory whose source files define the package, for
  packages that are not installed from a registry or index (Julia packages
  outside any manifest, Python editable installs and modules imported from
  the project directory).

`version` is a string, or `null` when the language does not provide one.
Paths are absolute. `loom` computes fingerprints from these paths itself;
kernels report locations only.

## Shared data

```json
"imports": [{"name": "wages", "file": "/abs/.loom/shared/3f…a1.arrow"}],
"exports": ["fit_table", "n"]
```

Before running `code`, the kernel reads each import's file and binds the
value to `name` in the session. After `code` has run without error, it
writes the value bound to each name in `exports` to a new file in the
scratch directory and lists the files in the response:

```json
"exports": [{"name": "fit_table", "file": "/abs/scratch/…arrow"}, {"name": "n", "file": "…"}]
```

The `exports` field of the response is present (possibly empty) whenever
the request had one. A failure to read an import, or to write an export,
fails the cell like an error raised by user code: `failed: true`, with an
`error` output whose text says which name failed and why (an unsupported
value, an undefined name, a missing package). When the code itself failed,
nothing is exported. Imported names are bindings created by the request and
appear in `changed`.

### What can be shared

Three kinds of values, and nothing else:

- **data frame**: R `data.frame` (including tibble and data.table); Python
  pandas or polars `DataFrame`; Julia any Tables.jl table (DataFrames.jl
  `DataFrame` in practice). Columns may hold integers, floats, logicals,
  strings, factors/categoricals, dates, and date-times, with missing values.
  A column of another type is an error naming the column. R row names and
  pandas indexes are dropped.
- **array**: R atomic vector, `matrix`, or `array` (numeric, integer,
  logical, or character, with no class other than `matrix`/`array`); Python
  `numpy.ndarray` with a numeric, boolean, or string dtype; Julia
  `AbstractArray` whose elements are `Real`, `Bool`, or `AbstractString`.
  Missing values (R `NA`, Julia `missing`, Python `None`) are an error; NaN
  is a number.
- **scalar**: R length-1 atomic vector without `dim` (numeric, integer,
  logical, or character); Python `int`, `float`, `bool`, `str`, or a numpy
  scalar of those kinds; Julia `Real`, `Bool`, or `AbstractString`.

Anything else (a list, an object with a class, a function) is an error
that names the type and lists what can be shared.

### File format

Every value is written as an Arrow IPC **stream** (not the file format),
with these keys in the schema's metadata:

| key | value |
|---|---|
| `loom.kind` | `dataframe`, `array`, or `scalar` |
| `loom.shape` | arrays only: the dimensions as a JSON list, e.g. `[3,4]`; a vector is `[n]` |

- A data frame is the table itself, one Arrow column per column.
- An array is a table with one column `values` holding the elements in
  column-major order (R and Julia order; numpy uses `order="F"`), of type
  int32, int64, float64, bool, or utf8.
- A scalar is a table with one column `value` and one row.

Reading gives: a data frame as R `data.frame`, pandas `DataFrame` (polars
when the request's options say `"dataframe": "polars"`), Julia
`DataFrames.DataFrame`; an array as an R vector (one dimension) or
`matrix`/`array`, a numpy array of the given shape, a Julia `Array`; a
scalar as a length-1 R vector, a Python `int`/`float`/`bool`/`str`, a Julia
scalar. Integer widths are kept (an R integer is int32 and arrives in Julia
as `Int32`).

The R kernel writes factor columns as strings: nanoarrow (0.9) cannot write
dictionary-encoded columns. It reads dictionaries as factors.

The packages used are R `nanoarrow`, Python `pyarrow` (and pandas or
polars for data frames), Julia `Arrow.jl` and `DataFrames.jl`, loaded only
when a request imports or exports. A missing package fails the cell with an
error that names it.

## Block mode

The code is split into top-level statements using the language's own parser
and the statements are executed one at a time, so that output appears next
to the statement that produced it.

### Source outputs

Before executing a statement, the kernel emits

```json
{"type": "source", "code": "..."}
```

Source outputs partition the code **by lines**:

- each source output consists of whole lines of the original code;
- lines that contain no statement (blank lines, comments) belong to the
  following statement; such lines after the last statement belong to the
  last statement;
- statements that share a line belong to the same source output;
- joining the `code` of all source outputs of a cell with `\n` reproduces
  the cell's code exactly.

If the code cannot be parsed, the kernel emits a single source output with
the whole code followed by an `error` output and sets `failed: true`.

### Output types

Every output produced while a statement runs is emitted after that
statement's source output, in the order it occurred:

| `type` | fields | produced by |
|---|---|---|
| `stream` | `name` (`stdout` or `stderr`), `text` | printing to stdout / stderr |
| `message` | `text` | R `message()`; Julia `@info`, `@debug` log records |
| `warning` | `text` | R `warning()`; Julia `@warn`, `@error` log records; Python `warnings.warn` |
| `display` | `data` | explicit display: Julia `display(x)`; a plot drawn on an R graphics device; a matplotlib figure (see below) |
| `value` | `data`, `repl_only` | the value of a top-level statement |
| `error` | `text` | an error raised by user code (message plus a short traceback) |

Consecutive `stream` outputs with the same `name` are merged. `text` fields
have no trailing newline.

### Matplotlib figures

A matplotlib figure is emitted after the last statement of the cell that
changed it and saved in its state at the end of the cell, so that
`fig, ax = plt.subplots()` followed by drawing statements gives one complete
figure. `plt.show()` emits the open figures at that point; figures closed by
user code are not emitted. All figures are closed when the cell ends.

### Values

Which statement values produce a `value` output:

- **Julia and Python**: every top-level statement whose value is not
  `nothing` / `None` and whose source does not end in `;`. The value of the
  last statement of the cell has `repl_only: false`; all others have
  `repl_only: true`. Matplotlib objects (figures, axes, lists of artists)
  produce no `value` output, since the figure itself is captured as a
  `display`.
- **R**: every visible value, all with `repl_only: false` (knitr's rule).
  Printing an object that draws on a graphics device (a ggplot object) yields
  a `display` output with the image; the `value` output is omitted when
  `print` writes no text.

`loom` shows `repl_only` values only in REPL style (`prompt: true`).

### MIME bundles

`data` is a dictionary keyed by MIME type:

| key | content |
|---|---|
| `text/plain` | the REPL representation, as `print` (R), `show(io, MIME"text/plain"(), x)` with `:limit => true` (Julia), `repr` (Python) would write it |
| `text/x-string` | present only when the value is a string: its contents, unquoted (R: elements of a character vector joined with `\n`) |
| `text/typst` | Typst markup: Julia `show(io, MIME"text/typst"(), x)`; R `loom_typst(x)` (an S3 generic the kernel defines, with methods for tinytable and gt objects); Python `x._repr_typst_()` |
| `image/svg+xml`, `image/png`, `application/pdf` | `{"file": "<absolute path in the scratch directory>"}` |

Rules:

- `text/plain` is always present for `value` outputs.
- Rich representations (`text/typst`, images) are computed only for
  `display` outputs and for `value` outputs with `repl_only: false`.
- If `text/typst` is available no image is produced.
- At most one image is produced per bundle: the kernel tries `fig-format`
  first and then the remaining formats in the order svg, png, pdf, and uses
  the first one the object supports.
- Image file names in the scratch directory are unique for the life of the
  process. `loom` moves the files away after each response.

### Figure size

The kernel makes figures created during the cell default to `fig-width` by
`fig-height` inches:

- Julia: Plots.jl default `size` and Makie theme `size` of
  `(96 * fig-width, 96 * fig-height)` units, when those packages are loaded;
- R: device width and height in inches (svglite when available, otherwise
  `svg()`; `png()` with `res = fig-dpi`; `pdf()`);
- Python: matplotlib `figure.figsize` and `savefig.dpi`.

## Inline mode

The code is a single expression whose value is inserted into running text.
The kernel evaluates it and emits exactly one output,

```json
{"type": "value", "repl_only": false, "data": {"text/plain": "...", "text/typst": "..."}}
```

where `text/plain` uses the language's printing conventions for text, not
REPL display: strings without quotes, numbers as `print` writes them
(Julia `sprint(print, x)`, R `paste(format(x), collapse = ", ")`, Python
`str(x)`). `text/typst` is included when available. No source outputs are
emitted. An error produces a single `error` output and `failed: true`.
