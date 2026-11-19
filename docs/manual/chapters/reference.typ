= Reference

== Configuration <sec-config>

`loom.toml`, in the directory of the root file, is optional; `loom init`
writes a commented copy. These are the defaults:

```toml
[execute]
fig-width = 7        # inches
fig-height = 5       # inches
fig-format = "svg"   # svg, png, or pdf
fig-dpi = 300        # resolution of raster figures
print-width = 80     # characters per line of printed output
error = false        # true: errors are output instead of failures

[julia]
command = "julia"
args = ["--startup-file=no", "--threads=1,1", "--project=@."]

[r]
command = "Rscript"
args = []

[python]
command = "python3"
args = []

[typst]
command = "typst"    # used by `loom render`
```

A kernel is started as `command args… script`, in the directory of the root
file. Use `args` to choose an environment, for example
`args = ["--startup-file=no", "--threads=1,1", "--project=../env"]` for
Julia; keep `--threads=1,1` (or another setting with an interactive thread)
so that loom can interrupt Julia cells. Unknown keys are errors.

== Arguments of `loom`

`#show: loom.with(...)` accepts:

/ `echo`, `output`, `warning`, `message`, `prompt`, `collapse`, `comment`: document-wide defaults for the cell options of the same names.
/ `columns`: a dictionary of functions, keyed by `margin` and `page`, that place content for the `column` option.
/ `scope`: a dictionary of definitions available to captions and to Typst produced by cells.
/ `style`: a dictionary that overrides the look of `code`, `output`, `error`, `warning`, and `prompt`, and the `spacing` between parts of a cell.

== Command line <sec-cli>

#table(
  columns: (auto, 1fr),
  stroke: none,
  table.hline(),
  table.header([*Command*], [*Effect*]),
  table.hline(stroke: 0.5pt),
  [`loom init [DIR]`], [write `.loom/loom.typ`, `.loom/cells.json`, and `loom.toml`],
  [`loom run FILE`], [run the cells whose results are out of date],
  [`loom run --force FILE`], [run every cell],
  [`loom render FILE [-- ARGS]`], [run, then `typst compile --input loom=strict FILE ARGS`],
  [`loom watch FILE`], [run after every save, keeping the kernels running],
  [`loom list FILE`], [sessions, their cells, and how many are up to date],
  [`loom clean FILE`], [remove results and figures the document no longer uses],
  [`loom kernels list`], [show running kernels of the project],
  [`loom kernels stop`], [stop them],
  table.hline(),
)

`-v` shows more detail. Only one loom command at a time can work on a
project; a second one reports which process holds it.

== Strict mode

The Typst library reads `sys.inputs.loom`. With `--input loom=strict`, any
cell that is not executed, stale, or failed stops the compilation with a
message naming the cell, as does a cell whose `label` refers to output it
did not produce. Without it, such cells are marked in the document.

== Kernel protocol

The kernels and loom communicate through newline-delimited JSON over a TCP
connection on the loopback interface, opened by the kernel and
authenticated with a random token. The protocol is specified in
`kernels/PROTOCOL.md` in the loom repository.
