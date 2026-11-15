= Figures, tables, and equations <sec-figures>

loom produces the same elements a hand-written document uses: Typst
`figure` elements with kind `image` or `table`. They are numbered together
with the document's other figures and tables, styled by its `figure` show
rules, and referenced with `@label`.

== Figures

A cell whose value can be shown as an image produces a figure: a Plots.jl
or Makie figure in Julia, a ggplot or base graphics plot in R, a matplotlib
figure in Python. With a `label` starting with `fig-`, or a `fig-cap`, the
image is placed in a numbered figure:

````typ
```julia
#| label: fig-sine
#| fig-cap: One period of the sine function.
using Plots
plot(sin, 0, 2π; label = "sin(x)", lw = 2)
```
````

```julia
#| label: fig-sine
#| fig-cap: One period of the sine function.
using Plots
plot(sin, 0, 2π; label = "sin(x)", lw = 2)
```

The text can now refer to @fig-sine. Without a label or a caption the image
is shown without a figure around it.

`fig-width` and `fig-height` set the size, in inches, at which the figure is
drawn; it is displayed at that width unless the text column is narrower.
`out-width` changes the displayed width alone, for example `out-width: 60%`.

A cell that draws several plots shows them all in one figure, one below the
other, or side by side with `layout-ncol`:

```r
#| label: fig-pair
#| fig-cap: Two plots from one R cell, with `layout-ncol: 2`.
#| layout-ncol: 2
#| fig-width: 3.2
#| fig-height: 2.6
library(ggplot2)
print(ggplot(mtcars, aes(wt, mpg)) + geom_point())
print(ggplot(mtcars, aes(factor(cyl))) + geom_bar())
```

== Subfigures

`fig-subcap` gives each image of a cell a caption of its own. The images
become subfigures of one figure, lettered (a), (b), and so on, and with a
label `fig-name` the subfigures are labeled `fig-name-1`, `fig-name-2`, in
order. The captions are written as a list, either over several lines or on
one line:

````typ
```python
#| label: fig-waves
#| fig-cap: Two waves.
#| fig-subcap:
#|   - "Sine"
#|   - "Cosine"
#| layout-ncol: 2
...
```
````

The same list on one line is `fig-subcap: ["Sine", "Cosine"]`. The cell:

```python
#| label: fig-waves
#| fig-cap: Two waves, drawn in one Python cell.
#| fig-subcap:
#|   - "Sine"
#|   - "Cosine"
#| layout-ncol: 2
#| fig-width: 3
#| fig-height: 2
#| echo: false
import math
import matplotlib.pyplot as plt
t = [k / 10 for k in range(63)]
fig1, ax1 = plt.subplots(); ax1.plot(t, [math.sin(x) for x in t])
fig2, ax2 = plt.subplots(); ax2.plot(t, [math.cos(x) for x in t]);
```

@fig-waves has two parts; @fig-waves-2 is the cosine. References to a
subfigure use the number of its figure, in the document's own numbering
style and language. Only the enclosing figure appears in a list of figures.
A cell must produce as many images as it has subcaptions; otherwise strict
mode stops the compilation and the preview marks the cell.

== Tables

A value with a Typst representation is inserted as Typst content. In Julia,
this is any type with a `show` method for `MIME"text/typst"`, such as the
tables of SummaryTables.jl. In R, tinytable objects. A `label` starting with
`tbl-`, or a `tbl-cap`, places the content in a numbered table figure:

```julia
#| label: tbl-summary
#| tbl-cap: A table produced by SummaryTables.jl.
using SummaryTables
Table([Cell("Variable", bold = true) Cell("Mean", bold = true) Cell("SD", bold = true);
       Cell("height") Cell("170.2") Cell("9.1");
       Cell("weight") Cell("68.4") Cell("12.5")])
```

```r
#| label: tbl-cars
#| tbl-cap: The first rows of `mtcars`, from tinytable.
library(tinytable)
tt(head(mtcars[, 1:5], 4))
```

@tbl-summary and @tbl-cars are ordinary Typst tables.

== Equations and other markup

With `output: asis`, the value of the cell (or, in R, its printed text) is
read as Typst markup. A string holding an equation becomes an equation, and
a label starting with `eq-` makes it referenceable:

```julia
#| output: asis
#| label: eq-ols
#| echo: false
"\$ hat(beta) = (X^top X)^(-1) X^top y \$"
```

@eq-ols was written by a Julia cell. The same works for any markup a cell
produces, for example a list built from data.

== Captions and the document's definitions

Captions and Typst output are evaluated with Typst's `eval`, which does not
see the definitions of your document. Pass the ones they use to `loom`:

```typ
#let pkg(name) = text(font: "DejaVu Sans Mono", name)
#show: loom.with(scope: (pkg: pkg))
```

A caption can then contain `#pkg("Plots")`.

== Margins and full-width figures <sec-columns>

Placing content outside the text column depends on the page layout, so loom
leaves it to the document. `column: margin` and `column: page` call
functions that the document passes to `loom`. With the marginalia package:

```typ
#import "@preview/marginalia:0.3.1" as marginalia
#show: loom.with(columns: (
  margin: body => marginalia.note(numbering: none, counter: none, body),
  page: body => marginalia.wideblock(body),
))
```

A cell with `column: margin` then puts its figure (or, without a figure, its
whole output) in the margin. A cell that asks for a column for which no
function was given is an error.

== Style

Code is shown as a `raw` block, so the document's `show raw` rules apply to
it, including the font and size. The boxes around code, text output, and
errors, and the color of prompts, are set with the `style` argument; each
entry is merged with the defaults listed at the top of `.loom/loom.typ`:

```typ
#show: loom.with(style: (
  code: (fill: luma(240), stroke: none),
  output: (color: luma(40)),
))
```

Cell output other than code is set in the document's own font, size, and
language, as they are where `#show: loom` is applied. Place `#show: loom`
after your template and text settings.
