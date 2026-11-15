// The loom manual. It is itself a loom document: every example runs when
// the manual is built.
//
//   loom render docs/manual/manual.typ -- docs/manual/loom-manual.pdf

#import ".loom/loom.typ": loom

#set document(title: "loom manual")
#set page(
  paper: "a4",
  margin: (x: 24mm, top: 24mm, bottom: 26mm),
  numbering: "1",
)
#set text(size: 10.5pt, lang: "en")
#set par(justify: true)
#set heading(numbering: "1.1")
#set math.equation(numbering: "(1)")
#show heading.where(level: 1): it => {
  pagebreak(weak: true)
  v(1.5em)
  it
  v(0.5em)
}
#show link: set text(fill: rgb("#1f4e9a"))

// Typst listings that show loom source, as opposed to cells.
#show raw.where(lang: "typ", block: true): it => block(
  width: 100%,
  fill: rgb("#f4f6fa"),
  stroke: (left: 2pt + rgb("#9fb3d9")),
  inset: 8pt,
  it,
)

#show: loom

#align(center)[
  #v(25%)
  #text(size: 28pt, weight: "bold")[loom]
  #v(0.5em)
  #text(size: 14pt)[Julia, R, and Python code in Typst documents]
  #v(1em)
  #text(size: 11pt)[Version 0.3.0]
]
#pagebreak()

#outline(depth: 2)

#include "chapters/start.typ"
#include "chapters/cells.typ"
#include "chapters/figures.typ"
#include "chapters/books.typ"
#include "chapters/sharing.typ"
#include "chapters/preview.typ"
#include "chapters/caching.typ"
#include "chapters/languages.typ"
#include "chapters/reference.typ"
