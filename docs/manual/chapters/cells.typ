= Cells

A cell is a fenced code block (three or more backticks) whose language tag
is `julia`, `r`, or `python`. Code blocks with any other tag are left alone.
Each statement of a cell is followed in the document by the output it
produced.

```r
x <- c(2, 3, 5, 7)
mean(x)
summary(x)
```

== Options

Options are written at the top of a cell, one per line, as `#| key: value`.
The `#|` lines are not part of the code: loom removes them before running
the cell, and they are not shown.

````typ
```python
#| echo: false
print("only the output is shown")
```
````

```python
#| echo: false
print("only the output is shown")
```

Options fall into two groups. Execution options change what the kernel
computes (the session, figure sizes, whether errors are expected); changing
one makes loom run the cell again. Display options change only how the
stored result is drawn, so editing them updates the preview at once, without
running anything. @tbl-options lists them all.

Values are written as in YAML: `true` and `false`, numbers, and strings,
which may be quoted. Captions are Typst markup. An unknown option, or a
value of the wrong type, is an error that names the file and line.

#figure(
  table(
    columns: (auto, auto, 1fr),
    align: (left, left, left),
    stroke: none,
    table.hline(),
    table.header([*Option*], [*Kind*], [*Meaning*]),
    table.hline(stroke: 0.5pt),
    [`eval`], [], [run the cell (`true`); `false` shows the code only],
    [`session`], [execution], [name of the session the cell runs in (@sec-sessions)],
    [`setup`], [execution], [run at the start of every session of the language],
    [`stateless`], [execution], [the cell creates and rebinds no variables (@sec-stateless)],
    [`import`, `export`], [execution], [names of values read from or written for other cells (@sec-sharing)],
    [`error`], [execution], [an error is part of the output instead of a failure],
    [`fig-width`, `fig-height`], [execution], [figure size in inches (7 by 5)],
    [`fig-format`], [execution], [`svg` (default), `png`, or `pdf`],
    [`fig-dpi`], [execution], [resolution of raster figures (300)],
    [`print-width`], [execution], [characters per line of printed output (80)],
    [`echo`], [display], [show the code (`true`)],
    [`output`], [display], [`true`, `false`, or `asis` (output is Typst markup)],
    [`include`], [display], [`false` hides code and output; the cell still runs],
    [`warning`, `message`], [display], [show warnings and messages (`true`)],
    [`prompt`], [display], [REPL style with prompts (`false`)],
    [`collapse`], [display], [code and text output in one box (`false`)],
    [`comment`], [display], [prefix for each line of text output],
    [`label`], [display], [`fig-…`, `tbl-…`, or `eq-…` (@sec-figures)],
    [`fig-cap`, `tbl-cap`], [display], [caption, in Typst markup],
    [`fig-subcap`], [display], [captions of the subfigures, as a list],
    [`out-width`], [display], [displayed width, such as `80%` or `3in`],
    [`layout-ncol`], [display], [number of columns for the figures of a cell],
    [`column`], [display], [`body`, `margin`, or `page` (@sec-columns)],
    table.hline(),
  ),
  caption: [Cell options.],
) <tbl-options>

== What a cell shows

A cell's output is the sequence of what each statement produced: text it
printed, messages and warnings, plots, and values. How values are chosen
follows the custom of each language.

In Julia and Python, as in Jupyter, only the value of the last statement is
shown, and a trailing semicolon hides it:

```julia
a = 2
b = a^10
b - 24
```

In R, as in knitr, every visible value is shown; assignments are invisible:

```r
y <- 1:3
y * 2
sum(y)
```

With `prompt: true`, a cell is shown as a REPL session. Every statement gets
a prompt, and the value of every statement is shown, as at the REPL:

```julia
#| prompt: true
a = 2
b = a^10
b - 24
```

```python
#| prompt: true
words = ["loom", "typst"]
len(words)
" ".join(words).upper()
```

A value that can be shown as an image becomes a figure, and a value with a
Typst representation becomes Typst content; @sec-figures and
@sec-languages describe both.

Printed text is laid out by the language itself, for a line of
`print-width` characters (80 by default). Set it in `loom.toml` to the
number of monospaced characters that fit your text column, so that tables
and summaries are not wrapped by Typst; a cell can override it:

```julia
#| print-width: 40
collect(1:30)'
```

== Inline code

Code in running text is written with single backticks and the language in
braces, as in Quarto: ```typ `{r} nrow(mtcars)` ``` becomes
`{r} nrow(mtcars)`, and ```typ `{python} round(2 ** 0.5, 4)` ``` becomes
`{python} round(2 ** 0.5, 4)`. Text is inserted without quotes, numbers as
the language prints them. A value with a Typst representation is inserted
as Typst content.

Inline code runs in the session of its chapter, so it sees the variables of
earlier cells: `x` from the first cell of this chapter has
`{r} length(x)` elements.

== Errors

An error raised by a cell stops the run: `loom run` reports the cell, its
file and line, and the error, and exits with a non-zero status. Results
computed before the error are kept. In the preview, the failing cell shows
the error.

When the error is the point of the example, mark the cell with
`error: true`; the error message becomes part of the output and the run
continues:

```julia
#| error: true
sqrt(-1.0)
```

== Code that is not run

`eval: false` shows a cell as a listing without running it:

```python
#| eval: false
while True:
    pass
```
