= Live preview <sec-preview>

The intended way to write a loom document is with two windows: an editor
with the Tinymist extension showing the document's preview, and a terminal
running

```sh
loom watch paper.typ
```

Tinymist compiles the document on every keystroke, from the text in the
editor. loom sees the files only when they are saved; it then runs the cells
that changed and rewrites `.loom/cells.json`, which Tinymist notices and
recompiles. Prose, captions, and display options update as you type; code
results update when you save.

== Telling current results from old ones

Between a keystroke and the end of the next run, the preview shows code
whose results are not yet known. The Typst library checks, for every cell,
that its stored result belongs to the code on screen and to the cells
before it, and marks those that do not:

- *not executed yet*: a new cell, or one you are editing. A cell with a
  `label` keeps showing its last result, marked as stale, so that a figure
  does not disappear while you change it.
- *stale*: the cell itself is unchanged, but an earlier cell of its session
  was edited, inserted, or deleted, so its result was computed from a state
  that no longer exists. Changing an execution option of the cell (such as
  `fig-width`) also makes it stale.
- *error*: the cell raised an error; the error is shown.

The checks use only the document and `.loom/cells.json`, so they work in
Tinymist, in `typst compile`, and in `typst watch` alike.

== While loom runs

During `loom watch`, results appear as each cell finishes, so a long
chapter fills in progressively. Saving again while cells run affects the
run only when it changes cells. If the cell being computed was edited, loom
interrupts it; if other cells changed, the run stops after the current
cell; then loom starts over with the new text. Saving prose leaves the run
alone.

In the terminal, Ctrl-C interrupts the cell being computed and keeps
watching. A second Ctrl-C, or one while nothing runs, stops loom and its
kernels.

== The final document

`typst compile --input loom=strict paper.typ` turns every mark into an
error, naming the cell. `loom render` runs the cells and then compiles this
way, so a PDF built with it contains only current results.
