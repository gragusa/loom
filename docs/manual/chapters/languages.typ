= Notes on each language <sec-languages>

Each language runs in its own kernel process, started the first time a run
needs it, in the directory of the root file. The command that starts it is
set in `loom.toml` (@sec-config).

== Julia

The kernel runs Julia with `--project=@.`, so the environment is the
nearest `Project.toml` above the document. It needs nothing from that
environment; packages the cells use must be in it.

Each session is a module. A cell's value is shown with
`show(io, MIME"text/plain"(), x)`, as at the REPL. The richest
representation available is used for the last value of a cell and for
explicit `display(x)` calls: Typst markup if the type has a `show` method
for `MIME"text/typst"`, otherwise an image in the requested format, otherwise
text. Any type can opt in:

```julia
struct Badge
    text::String
end
Base.show(io::IO, ::MIME"text/typst", b::Badge) =
    print(io, "#box(fill: rgb(\"#dbe8f9\"), inset: 3pt, radius: 3pt)[", b.text, "]")
Badge("written by Julia")
```

Log records become messages (`@info`) and warnings (`@warn`, `@error`), and
are shown in the order they occurred:

```julia
@info "a message" n = 3
@warn "a warning"
```

The figure size options are applied as defaults for Plots.jl and Makie when
those packages are loaded; a size given explicitly in the code takes
precedence.

== R

The kernel needs the packages `jsonlite` and `evaluate`; the second is the
engine knitr uses, and the output follows knitr's rules. Each session is an
environment whose parent is the global environment, so packages attached
with `library` are visible from every session.

Plots drawn with base graphics or printed ggplot objects become figures. A
plot that only adds to the previous one (`abline` after `plot`) replaces it,
so a cell produces one figure per plot, as in knitr. Messages and warnings
are shown where they occur, unless turned off with `message: false` or
`warning: false`:

```r
x <- log(-1)
message("computing...")
x
```

Values with a Typst representation are produced by the generic
`loom_typst(x)`, which the kernel defines with methods for tinytable and,
when the installed version of gt can export Typst, for gt. A method defined
in a cell applies to that session:

```r
loom_typst.temperature <- function(x, ...) {
  sprintf("$%s degree C$", format(unclass(x)))
}
structure(21.5, class = "temperature")
```

== Python

The kernel uses only the standard library. Each session is a dictionary of
globals, as in a module. The last value of a cell is shown with `repr`, and
an object can offer richer representations with the methods
`_repr_typst_`, `_repr_svg_`, and `_repr_png_`:

```python
class Fraction:
    def __init__(self, num, den):
        self.num, self.den = num, den
    def __repr__(self):
        return f"{self.num}/{self.den}"
    def _repr_typst_(self):
        return f"${self.num} / {self.den}$"

Fraction(3, 4)
```

Matplotlib figures become figures without a call to `show`. A figure is
placed after the last statement that changed it, so creating the axes and
drawing on them in separate statements gives one figure:

```python
#| label: fig-matplotlib
#| fig-cap: A matplotlib figure built over several statements.
import math
import matplotlib.pyplot as plt
fig, ax = plt.subplots()
ax.plot([math.cos(t / 10) for t in range(100)])
ax.set_xlabel("t");
```
