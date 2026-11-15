= Books and sessions <sec-sessions>

== Chapters

A long document is usually a root file that includes one file per chapter:

```typ
#import ".loom/loom.typ": loom
#show: loom

#include "preamble.typ"
#include "chapters/data.typ"
#include "chapters/models.typ"
```

Every `#include` in the root file starts a chapter, named after the
included file (`data`, `models`). Files included from a chapter file belong
to that chapter. Cells written in the root file itself form a chapter named
after the root file. A single-file document is a single chapter.

`#show: loom` in the root file applies to the whole book. A chapter file may
also enable loom, so that Tinymist can preview it on its own; enabling it
twice does no harm.

== Sessions

A session is the namespace in which cells run: a Julia module, an R
environment, a Python dictionary of globals. Each chapter has its own
session for each language, so the variables of one chapter are not visible
in another. This manual has a Julia session per chapter; the variable `b`,
defined in the chapter on cells, does not exist here:

```julia
@isdefined(b)
```

Packages are loaded once per kernel process. A package loaded in one chapter
is therefore already loaded in the next, but each session must still bring
it into scope with `using`, `library`, or `import`.

Cells that should share a session across chapters name it with the
`session` option. Cells with the same name, in any chapter, run in one
session in document order:

```julia
#| session: shared-example
total = 40;
```

```julia
#| session: shared-example
total + 2
```

== Setup code

Setup cells run before every other cell of their language and are replayed
at the start of every session of that language. Packages and functions that
the whole document uses belong there. A cell becomes a setup cell with
`setup: true`, and every cell in a file named `preamble.typ` is a setup cell
unless it says `setup: false`.

````typ
```julia
#| setup: true
#| include: false
using Statistics, Random
Random.seed!(1)
```
````

Because every session starts by replaying the setup code, changing a setup
cell makes every cell of that language run again.

== Several languages in one document

Julia, R, and Python cells can be mixed freely. Each language has its own
kernel and its own sessions, so variables are not shared between languages;
data that two languages need is passed through files. In this chapter, R
sees its own `total`, not Julia's:

```r
exists("total")
```
