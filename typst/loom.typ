// loom.typ: renders the results of code cells executed by loom.
//
// This file is written by loom into `.loom/` and rewritten by every loom
// command; edit the copy in the loom repository, not this one.
//
// Usage in the root file of a document:
//
//   #import ".loom/loom.typ": loom
//   #show: loom
//
// Cells are raw blocks tagged `julia`, `r`, or `python`, with optional
// `#| key: value` option lines at the top, and inline code `{julia} expr`.
// Results come from `cells.json`, which loom writes next to this file.
// Compiling with `--input loom=strict` turns any cell without a current
// result into an error.

// loom finds cells with the parser of Typst 0.15, and this library must see
// the same text for each code block.
#if sys.version < version(0, 15, 0) {
  panic("loom needs Typst 0.15 or later; this is Typst " + str(sys.version))
}

#let _data = json("cells.json")
#let _strict = sys.inputs.at("loom", default: "draft") == "strict"
#let _langs = ("julia", "r", "python")

// Options that change execution; they must match the stored result.
#let _exec-keys = ("session", "setup", "stateless", "import", "export", "error", "fig-width", "fig-height", "fig-format", "fig-dpi")

// Text settings in effect where `#show: loom` was applied. Content produced
// by a show rule on `raw` inherits raw's own text settings (monospace font,
// 0.8em, English, no justification); cell output other than code is reset
// to these.
#let _outer = state("loom-outer-text", none)

#let _restore(body, inline: false) = context {
  let b = _outer.get()
  if b == none { return body }
  set text(
    font: b.font,
    size: b.size,
    lang: b.lang,
    region: b.region,
    hyphenate: b.hyphenate,
    overhang: b.overhang,
    cjk-latin-spacing: b.cjk-latin-spacing,
  )
  // A paragraph setting inside inline content would break the paragraph.
  show: if inline { it => it } else { it => { set par(justify: b.justify); it } }
  body
}

#let _prompts = (
  julia: ("julia> ", "       "),
  r: ("> ", "+ "),
  python: (">>> ", "... "),
)

#let _default-style = (
  code: (fill: luma(246), stroke: 0.5pt + luma(220), inset: 8pt, radius: 2pt),
  output: (fill: none, stroke: (left: 1.5pt + luma(220)), inset: (x: 8pt, y: 4pt), radius: 0pt, color: luma(70)),
  error: (fill: rgb("#fff0f0"), stroke: 0.5pt + rgb("#f0b0b0"), inset: 8pt, radius: 2pt, color: rgb("#b00000")),
  warning: (color: rgb("#9a5b00")),
  prompt: (color: rgb("#389826")),
  spacing: 0.6em,
)

// ── Options ──────────────────────────────────────────────────────────────────

// Same rule as `split_options` in loom's src/options.rs: option lines are the
// leading lines whose first non-blank characters are `#|`.
#let _split(text) = {
  let lines = text.split("\n")
  let n = 0
  for l in lines {
    if l.trim(at: start).starts-with("#|") { n += 1 } else { break }
  }
  let rest = lines.slice(n)
  (options: lines.slice(0, n), code: if rest.len() == 0 { "" } else { rest.join("\n") })
}

#let _unquote(v) = {
  for q in ("\"", "'") {
    if v.len() >= 2 and v.starts-with(q) and v.ends-with(q) { return v.slice(1, -1) }
  }
  v
}

// Option values are strings, except a list written over several lines
// (`#| fig-subcap:` followed by `#|   - item` lines), which is an array.
#let _parse-options(lines) = {
  let o = (:)
  let list-key = none
  for l in lines {
    let body = l.trim(at: start).slice(2).trim()
    if body == "" { continue }
    if body.starts-with("-") and list-key != none {
      o.at(list-key).push(_unquote(body.slice(1).trim()))
      continue
    }
    let i = body.position(":")
    if i == none { continue }
    let key = body.slice(0, i).trim()
    let value = body.slice(i + 1).trim()
    if value == "" and key == "fig-subcap" {
      o.insert(key, ())
      list-key = key
    } else {
      o.insert(key, value)
      list-key = none
    }
  }
  o
}

// A list written on one line, as `["First", 'Second', third]`; the same rule
// as `parse_flow_list` in loom's src/options.rs.
#let _parse-flow(v) = {
  let v = v.trim()
  if not (v.starts-with("[") and v.ends-with("]")) { return none }
  let cs = v.slice(1, -1).clusters()
  let n = cs.len()
  let items = ()
  let i = 0
  while i < n {
    while i < n and cs.at(i).trim() == "" { i += 1 }
    if i >= n { break }
    let c = cs.at(i)
    if c == "\"" or c == "'" {
      let j = i + 1
      while j < n and cs.at(j) != c { j += 1 }
      items.push(cs.slice(i + 1, j).join(default: ""))
      i = j + 1
      while i < n and cs.at(i).trim() == "" { i += 1 }
      i += 1
    } else {
      let j = i
      while j < n and cs.at(j) != "," { j += 1 }
      items.push(cs.slice(i, j).join(default: "").trim())
      i = j + 1
    }
  }
  items
}


#let _bool(o, key, default) = {
  let v = o.at(key, default: none)
  if v == none { default } else { v == "true" }
}

#let _exec-options(o) = {
  let d = (:)
  for k in _exec-keys {
    if k in o { d.insert(k, o.at(k)) }
  }
  d
}

#let _same-dict(a, b) = a.len() == b.len() and a.keys().all(k => k in b and b.at(k) == a.at(k))

// ── Building blocks ──────────────────────────────────────────────────────────

#let _trim-blank-lines(text) = {
  let lines = text.split("\n")
  while lines.len() > 0 and lines.first().trim() == "" { lines = lines.slice(1) }
  while lines.len() > 0 and lines.last().trim() == "" { lines = lines.slice(0, -1) }
  lines.join("\n", default: "")
}

// Code is re-emitted as a raw block labeled <loom-echo>; the show rule passes
// such blocks through unchanged, which stops it from treating them as cells.
#let _echo(code, lang) = [#raw(code, lang: lang, block: true) <loom-echo>]

#let _prompted(chunk, lang, style) = {
  let (first, rest) = _prompts.at(lang)
  show raw.line: l => {
    text(fill: style.prompt.color, if l.number == 1 { first } else { rest })
    l.body
  }
  _echo(chunk, lang)
}

#let _box(s, body) = block(
  width: 100%,
  fill: s.fill,
  stroke: s.stroke,
  inset: s.inset,
  radius: s.radius,
  body,
)

#let _status-banner(status, style) = {
  let (msg, color) = if status == "pending" {
    ("not executed yet: run loom", luma(110))
  } else if status == "stale" {
    ("stale: the code changed since this output was computed", rgb("#9a5b00"))
  } else {
    ("error raised by this cell", rgb("#b00000"))
  }
  block(above: style.spacing, below: 0.3em, text(size: 0.75em, fill: color, [loom · #msg]))
}

#let _eval-markup(text, scope) = eval(text, mode: "markup", scope: scope)

#let _image(file, entry, out-width) = {
  let path = file
  if out-width != none {
    image(path, width: out-width)
  } else {
    // Displayed at the requested figure width, but never wider than the
    // container (text column, margin note, ...).
    let w = entry.at("fig-width") * 1in
    layout(size => align(center, image(path, width: calc.min(w, size.width))))
  }
}

#let _image-mimes = ("image/svg+xml", "image/png", "application/pdf")

// Kind of the figures made from the images of a cell with `fig-subcap`.
#let _subfigure-kind = "loom-subfigure"

// Classify one rich output (value or display) into a piece to render.
#let _rich-piece(data, asis) = {
  if asis {
    let t = data.at("text/typst", default: data.at("text/x-string", default: data.at("text/plain", default: "")))
    return (kind: "markup", text: t)
  }
  if "text/typst" in data { return (kind: "typst", text: data.at("text/typst")) }
  for m in _image-mimes {
    if m in data { return (kind: "image", file: data.at(m).file) }
  }
  (kind: "text", text: data.at("text/plain", default: ""))
}

// Turn kernel outputs into a list of pieces: code, text (with a tone),
// error, image, typst, markup. Consecutive code and consecutive text pieces
// are merged.
#let _pieces(outputs, s) = {
  let pieces = ()
  let push(pieces, p) = {
    if pieces.len() > 0 {
      let last = pieces.last()
      if p.kind == "code" and last.kind == "code" and not s.prompt {
        pieces.last().chunks.push(p.chunks.first())
        return pieces
      }
      if p.kind == "text" and last.kind == "text" and last.tone == p.tone {
        pieces.last().text += "\n" + p.text
        return pieces
      }
    }
    pieces.push(p)
    pieces
  }
  let show-output = s.output != false and s.output != "false"
  let asis = s.output == "asis"
  for o in outputs {
    let t = o.type
    if t == "source" {
      if s.echo { pieces = push(pieces, (kind: "code", chunks: (o.code,))) }
    } else if t == "stream" {
      if show-output {
        if asis and o.name == "stdout" {
          pieces = push(pieces, (kind: "markup", text: o.text))
        } else {
          pieces = push(pieces, (kind: "text", tone: if o.name == "stderr" { "warning" } else { "plain" }, text: o.text))
        }
      }
    } else if t == "message" {
      if show-output and s.message { pieces = push(pieces, (kind: "text", tone: "plain", text: o.text)) }
    } else if t == "warning" {
      if show-output and s.warning { pieces = push(pieces, (kind: "text", tone: "warning", text: o.text)) }
    } else if t == "error" {
      pieces = push(pieces, (kind: "error", text: o.text))
    } else if t == "value" or t == "display" {
      if show-output and not (o.at("repl_only", default: false) and not s.prompt) {
        let p = _rich-piece(o.data, asis)
        pieces = push(pieces, if p.kind == "text" { (..p, tone: "plain") } else { p })
      }
    }
  }
  pieces
}

#let _comment-lines(text, comment) = {
  if comment == none or comment == "" { return text }
  text.split("\n").map(l => comment + " " + l).join("\n")
}

#let _render-text(p, s, style) = {
  let color = if p.tone == "warning" { style.warning.color } else { style.output.color }
  set text(fill: color)
  raw(_comment-lines(p.text, s.comment), block: true)
}

#let _render-code(p, lang, s, style) = {
  if s.prompt {
    for (i, c) in p.chunks.enumerate() {
      let t = _trim-blank-lines(c)
      if t != "" { _prompted(t, lang, style) }
    }
  } else {
    let t = _trim-blank-lines(p.chunks.join("\n"))
    if t != "" { _echo(t, lang) }
  }
}

// ── Cells ────────────────────────────────────────────────────────────────────

#let _resolve(o, cfg) = {
  let prompt = _bool(o, "prompt", cfg.prompt)
  (
  echo: _bool(o, "echo", cfg.echo),
  output: o.at("output", default: cfg.output),
  included: _bool(o, "include", true),
  warning: _bool(o, "warning", cfg.warning),
  message: _bool(o, "message", cfg.message),
  prompt: prompt,
  // A REPL transcript reads as one block, so prompt implies collapse.
  collapse: _bool(o, "collapse", cfg.collapse or prompt),
  comment: { let c = o.at("comment", default: none); if c == none { cfg.comment } else { _unquote(c) } },
  label: { let l = o.at("label", default: none); if l == none { none } else { _unquote(l) } },
  fig-cap: { let c = o.at("fig-cap", default: none); if c == none { none } else { _unquote(c) } },
  tbl-cap: { let c = o.at("tbl-cap", default: none); if c == none { none } else { _unquote(c) } },
  out-width: { let w = o.at("out-width", default: none); if w == none { none } else { eval(_unquote(w)) } },
  column: _unquote(o.at("column", default: "body")),
  layout-ncol: int(o.at("layout-ncol", default: "1")),
  fig-subcap: {
    let v = o.at("fig-subcap", default: none)
    if type(v) == array { v } else if type(v) == str { _parse-flow(v) } else { none }
  },
  )
}

#let _describe(lang, code) = {
  let first = code.split("\n").find(l => l.trim() != "")
  lang + " cell `" + if first == none { "" } else { first.trim() } + "`"
}

#let _render-cell-content(code, lang, o, entry, status, cfg) = {
  let s = _resolve(o, cfg)
  let style = cfg.style
  // An edited cell has no result yet; a labeled one shows the last result
  // its label had, so that a figure does not vanish while it is edited.
  if entry == none and s.label != none and s.label in _data.at("labels", default: (:)) {
    entry = _data.labels.at(s.label)
    status = "stale"
  }

  if _strict and status != "ok" {
    panic("loom: " + _describe(lang, code) + " is " + status + "; run `loom run` (strict mode)")
  }
  if not s.included and status != "error" { return }

  let banner = if status != "ok" { _status-banner(status, style) }
  if entry == none {
    banner
    if s.echo { _box(style.code, _echo(_trim-blank-lines(code), lang)) }
    return
  }

  let pieces = _pieces(entry.outputs, s)
  let images = pieces.filter(p => p.kind == "image")
  let label = s.label
  let fig-label = label != none and label.starts-with("fig-")
  let tbl-label = label != none and label.starts-with("tbl-")
  let eq-label = label != none and label.starts-with("eq-")
  let wants-figure = fig-label or s.fig-cap != none or s.fig-subcap != none
  let tables = pieces.filter(p => p.kind == "typst")
  let wants-table = tbl-label or s.tbl-cap != none
  let markups = pieces.filter(p => p.kind == "markup")

  let subcap-mismatch = s.fig-subcap != none and images.len() > 0 and s.fig-subcap.len() != images.len()
  if _strict {
    if subcap-mismatch {
      panic("loom: " + _describe(lang, code) + " has " + str(s.fig-subcap.len()) + " subcaptions for " + str(images.len()) + " images")
    }
    if fig-label and images.len() == 0 { panic("loom: " + _describe(lang, code) + " has label " + label + " but produced no image") }
    if tbl-label and tables.len() == 0 { panic("loom: " + _describe(lang, code) + " has label " + label + " but produced no Typst table") }
    if eq-label and markups.len() == 0 { panic("loom: " + _describe(lang, code) + " has label " + label + " but has no `output: asis` result") }
  }

  let column-hook(body) = {
    if s.column == "body" { return body }
    let hook = cfg.columns.at(s.column, default: none)
    if hook == none {
      panic("loom: " + _describe(lang, code) + " asks for `column: " + s.column + "`, but no hook is set; pass `columns: (" + s.column + ": body => ...)` to `loom.with(...)`")
    }
    hook(body)
  }

  let subfigures(imgs) = {
    let subs = imgs.enumerate().map(((k, p)) => {
      let cap = s.fig-subcap.at(k, default: none)
      let f = figure(
        _image(p.file, entry, s.out-width),
        caption: if cap != none { _eval-markup(cap, cfg.scope) },
        kind: _subfigure-kind,
        supplement: none,
        numbering: "a",
        outlined: false,
      )
      if fig-label { [#f #std.label(label + "-" + str(k + 1))] } else { f }
    })
    // Subfigures are lettered afresh in every figure.
    counter(figure.where(kind: _subfigure-kind)).update(0)
    grid(columns: (1fr,) * calc.max(s.layout-ncol, 1), gutter: 0.8em, ..subs)
  }

  let figure-of(imgs) = {
    let body = if s.fig-subcap != none {
      subfigures(imgs)
    } else if imgs.len() == 1 {
      _image(imgs.first().file, entry, s.out-width)
    } else if s.layout-ncol > 1 {
      grid(
        columns: (1fr,) * s.layout-ncol,
        gutter: 0.8em,
        ..imgs.map(p => _image(p.file, entry, s.out-width)),
      )
    } else {
      stack(spacing: 0.5em, ..imgs.map(p => _image(p.file, entry, s.out-width)))
    }
    if wants-figure {
      let f = figure(
        body,
        kind: image,
        caption: if s.fig-cap != none { _eval-markup(s.fig-cap, cfg.scope) },
      )
      if fig-label { [#f #std.label(label)] } else { f }
    } else {
      align(center, body)
    }
  }

  let table-of(p) = {
    let body = _eval-markup(p.text, cfg.scope)
    if wants-table {
      let f = figure(
        body,
        kind: table,
        caption: if s.tbl-cap != none { _eval-markup(s.tbl-cap, cfg.scope) },
      )
      if tbl-label { [#f #std.label(label)] } else { f }
    } else { body }
  }

  let out = ()
  let last-image = pieces.enumerate().filter(((k, p)) => p.kind == "image").map(((k, p)) => k).at(-1, default: -1)
  let i = 0
  while i < pieces.len() {
    let p = pieces.at(i)
    if s.collapse and p.kind in ("code", "text") {
      // A run of code and text output shares one box.
      let parts = ()
      while i < pieces.len() and pieces.at(i).kind in ("code", "text") {
        let q = pieces.at(i)
        parts.push(if q.kind == "code" { _render-code(q, lang, s, style) } else { _render-text(q, s, style) })
        i += 1
      }
      out.push(_box(style.code, stack(spacing: 0.5em, ..parts)))
      continue
    } else if p.kind == "code" {
      out.push(_box(style.code, _render-code(p, lang, s, style)))
    } else if p.kind == "text" {
      out.push(_box(style.output, _render-text(p, s, style)))
    } else if p.kind == "error" {
      out.push(_box(style.error, { set text(fill: style.error.color); raw(p.text, block: true) }))
    } else if p.kind == "image" {
      // All images of a cell form one figure, placed after the code that
      // drew the last of them.
      if i == last-image {
        out.push(column-hook(figure-of(images)))
      }
    } else if p.kind == "typst" {
      out.push(column-hook(table-of(p)))
    } else if p.kind == "markup" {
      let m = _eval-markup(p.text.trim(), cfg.scope)
      out.push(if eq-label { [#m #std.label(label)] } else { m })
    }
    i += 1
  }

  if subcap-mismatch {
    banner = [#banner #text(size: 0.75em, fill: rgb("#9a5b00"))[loom · #s.fig-subcap.len() subcaptions for #images.len() images]]
  }
  banner
  // Full-width blocks: inside a `stack`, figures would shrink to their
  // content and lose their centering.
  out.map(x => block(width: 100%, spacing: style.spacing, x)).join()
}

#let _render-cell(code, lang, o, entry, status, cfg) = _restore(_render-cell-content(code, lang, o, entry, status, cfg))

// ── Document-order bookkeeping ───────────────────────────────────────────────

// One state, advanced by a pure function at every evaluated cell, decides
// whether each cell's stored result is current. Because no update depends on
// a contextual value, the whole document is resolved in one layout pass.
//
// The rules must match `typst_predecessors` in loom's src/plan.rs, which
// records for every result the token (lookup key and occurrence number) of
// the cell expected before it in its session (the last earlier cell of the
// session that is not stateless, or for a session's first cell the last
// setup cell of its language) and, for each import, the token of the latest
// earlier cell exporting that name.
#let _cells = state("loom-cells", (counts: (:), last: (:), dirty: (:), default: (:), exports: (:), current: none))

// A comma-separated list of names, split as loom's src/options.rs splits it.
#let _names(o, key) = {
  let v = o.at(key, default: none)
  if v == none { return () }
  _unquote(v).split(",").map(n => n.trim()).filter(n => n != "")
}

#let _advance(d, key, lang, o) = {
  let n = d.counts.at(key, default: 0) + 1
  d.counts.insert(key, n)
  let token = key + "\u{1}" + str(n)
  let results = _data.cells.at(key, default: ())
  let entry = if n <= results.len() { results.at(n - 1) } else { none }
  let stateless = o.at("stateless", default: "false") == "true"
  let explicit = o.at("session", default: none)
  let setup = o.at("setup", default: "false") == "true"
  // A cell without a result (just written or edited) is attributed to the
  // session of the latest cell of its language that used a default session.
  let name = if entry != none { entry.session } else if setup { "setup" } else if explicit != none {
    _unquote(explicit)
  } else { d.default.at(lang, default: none) }
  let session = if name == none { none } else { lang + "/" + name }
  let setup-session = lang + "/setup"
  let expected = if session != none and session in d.last { d.last.at(session) } else {
    d.last.at(setup-session, default: none)
  }
  let status = if entry == none {
    "pending"
  } else if entry.status == "error" {
    "error"
  } else if not _same-dict(entry.exec, _exec-options(o)) {
    "stale"
  } else if entry.after != expected {
    "stale"
  } else if d.dirty.at(session, default: false) or d.dirty.at(setup-session, default: false) {
    "stale"
  } else if _names(o, "import").any(name => {
    // The cell an import reads from must be the one the result was computed
    // with, and must itself be current.
    let source = d.exports.at(name, default: none)
    source == none or not source.ok or entry.at("imports", default: (:)).at(name, default: none) != source.token
  }) {
    "stale"
  } else { "ok" }
  if entry != none and explicit == none and not setup and entry.session != "setup" {
    d.default.insert(lang, entry.session)
  }
  if session != none and not stateless {
    d.last.insert(session, token)
    if status != "ok" { d.dirty.insert(session, true) }
  }
  for name in _names(o, "export") {
    d.exports.insert(name, (token: token, ok: status == "ok"))
  }
  d.current = (status: status, entry: entry)
  d
}

#let _render-block(it, cfg) = {
  let parts = _split(it.text)
  let o = _parse-options(parts.options)
  let code = parts.code
  if not _bool(o, "eval", true) {
    if _bool(o, "echo", cfg.echo) and _bool(o, "include", true) {
      return _restore(_box(cfg.style.code, _echo(_trim-blank-lines(code), it.lang)))
    }
    return
  }
  let key = "b:" + it.lang + ":" + code
  let lang = it.lang
  _cells.update(d => _advance(d, key, lang, o))
  context {
    let c = _cells.get().current
    _render-cell(code, lang, o, c.entry, c.status, cfg)
  }
}

#let _inline-pattern = regex("^\{(julia|r|python)\}\s")

#let _render-inline(it, cfg) = {
  let m = it.text.match(_inline-pattern)
  let lang = m.captures.first()
  let code = it.text.slice(m.end).trim()
  let key = "i:" + lang + ":" + code
  _cells.update(d => _advance(d, key, lang, (:)))
  context {
    let c = _cells.get().current
    let (entry, status) = (c.entry, c.status)
    if _strict and status != "ok" {
      panic("loom: inline " + lang + " code `" + code + "` is " + status + "; run `loom run` (strict mode)")
    }
    show: _restore.with(inline: true)
    if status == "pending" or status == "stale" {
      box(fill: luma(235), inset: (x: 2pt), outset: (y: 2pt), radius: 2pt, text(fill: luma(110), raw(code)))
    } else if status == "error" {
      box(fill: rgb("#fff0f0"), inset: (x: 2pt), outset: (y: 2pt), radius: 2pt, text(fill: rgb("#b00000"), raw(code)))
    } else {
      let value = entry.outputs.find(o => o.type == "value")
      if value != none {
        let d = value.data
        if "text/typst" in d { _eval-markup(d.at("text/typst"), cfg.scope) } else { d.at("text/plain") }
      }
    }
  }
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Enable loom for the rest of the document: `#show: loom`, or
/// `#show: loom.with(echo: false, columns: (margin: body => ...))`.
///
/// - echo, output, warning, message, prompt, collapse, comment: document-wide
///   defaults for the cell options of the same names.
/// - columns: functions that place content outside the text column, keyed
///   by the values of the `column` cell option (`margin`, `page`).
/// - scope: definitions available to captions and Typst output, which are
///   evaluated with `eval`.
/// - style: overrides for the look of code, output, error, warning, and
///   prompt; merged into the defaults one level deep.
#let loom(
  echo: true,
  output: true,
  warning: true,
  message: true,
  prompt: false,
  collapse: false,
  comment: none,
  columns: (:),
  scope: (:),
  style: (:),
  doc,
) = {
  let merged = _default-style
  for (k, v) in style {
    merged.insert(k, if type(v) == dictionary and k in merged { merged.at(k) + v } else { v })
  }
  let cfg = (
    echo: echo,
    output: if output == true { true } else if output == false { false } else { output },
    warning: warning,
    message: message,
    prompt: prompt,
    collapse: collapse,
    comment: comment,
    columns: columns,
    scope: scope,
    style: merged,
  )
  context _outer.update((
    font: text.font,
    size: text.size,
    lang: text.lang,
    region: text.region,
    hyphenate: text.hyphenate,
    overhang: text.overhang,
    cjk-latin-spacing: text.cjk-latin-spacing,
    justify: par.justify,
  ))
  // Subfigures: captioned "(a) text", and referenced as the parent figure's
  // number followed by the letter, in the document's numbering and language.
  show figure.caption.where(kind: _subfigure-kind): it => context [(#it.counter.display("a")) #it.body]
  show ref: it => {
    let el = it.element
    if el == none or el.func() != figure or el.kind != _subfigure-kind { return it }
    context {
      let loc = el.location()
      let parent = query(figure.where(kind: image).before(loc)).last()
      let n = numbering(parent.numbering, ..counter(figure.where(kind: image)).at(parent.location()))
      let letter = numbering("a", ..counter(figure.where(kind: _subfigure-kind)).at(loc))
      link(loc, [#parent.supplement~#n#letter])
    }
  }
  show raw.where(block: true): it => {
    if it.lang not in _langs or it.at("label", default: none) == <loom-echo> { return it }
    _render-block(it, cfg)
  }
  show raw.where(block: false): it => {
    if it.lang != none or it.text.match(_inline-pattern) == none { return it }
    _render-inline(it, cfg)
  }
  doc
}
