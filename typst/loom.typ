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

#let _data = json("cells.json")
#let _strict = sys.inputs.at("loom", default: "draft") == "strict"
#let _langs = ("julia", "r", "python")

// Options that change execution; they must match the stored result.
#let _exec-keys = ("session", "setup", "error", "fig-width", "fig-height", "fig-format", "fig-dpi")

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

#let _parse-options(lines) = {
  let o = (:)
  for l in lines {
    let body = l.trim(at: start).slice(2).trim()
    if body == "" { continue }
    let i = body.position(":")
    if i == none { continue }
    o.insert(body.slice(0, i).trim(), body.slice(i + 1).trim())
  }
  o
}

#let _unquote(v) = {
  for q in ("\"", "'") {
    if v.len() >= 2 and v.starts-with(q) and v.ends-with(q) { return v.slice(1, -1) }
  }
  v
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
  )
}

#let _describe(lang, code) = {
  let first = code.split("\n").find(l => l.trim() != "")
  lang + " cell `" + if first == none { "" } else { first.trim() } + "`"
}

#let _render-cell-content(code, lang, o, entry, cfg) = {
  let s = _resolve(o, cfg)
  let style = cfg.style
  let status = if entry == none {
    "pending"
  } else if entry.status == "error" {
    "error"
  } else if not _same-dict(entry.exec, _exec-options(o)) {
    "stale"
  } else { "ok" }

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
  let wants-figure = fig-label or s.fig-cap != none
  let tables = pieces.filter(p => p.kind == "typst")
  let wants-table = tbl-label or s.tbl-cap != none
  let markups = pieces.filter(p => p.kind == "markup")

  if _strict {
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

  let figure-of(imgs) = {
    let body = if imgs.len() == 1 {
      _image(imgs.first().file, entry, s.out-width)
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
  let figure-done = false
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
      // All images of a cell form one figure, placed where the first appears.
      if not figure-done {
        out.push(column-hook(figure-of(images)))
        figure-done = true
      }
    } else if p.kind == "typst" {
      out.push(column-hook(table-of(p)))
    } else if p.kind == "markup" {
      let m = _eval-markup(p.text.trim(), cfg.scope)
      out.push(if eq-label { [#m #std.label(label)] } else { m })
    }
    i += 1
  }

  banner
  // Full-width blocks: inside a `stack`, figures would shrink to their
  // content and lose their centering.
  out.map(x => block(width: 100%, spacing: style.spacing, x)).join()
}

#let _render-cell(code, lang, o, entry, cfg) = _restore(_render-cell-content(code, lang, o, entry, cfg))

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
  // Identical cells are told apart by their order of appearance.
  let occurrence = counter("loom:" + key)
  occurrence.step()
  context {
    let n = occurrence.get().first()
    let results = _data.cells.at(key, default: ())
    let entry = if n <= results.len() { results.at(n - 1) } else { none }
    _render-cell(code, it.lang, o, entry, cfg)
  }
}

#let _inline-pattern = regex("^\{(julia|r|python)\}\s")

#let _render-inline(it, cfg) = {
  let m = it.text.match(_inline-pattern)
  let lang = m.captures.first()
  let code = it.text.slice(m.end).trim()
  let key = "i:" + lang + ":" + code
  let occurrence = counter("loom:" + key)
  occurrence.step()
  context {
    let n = occurrence.get().first()
    let results = _data.cells.at(key, default: ())
    let entry = if n <= results.len() { results.at(n - 1) } else { none }
    let status = if entry == none { "pending" } else if entry.status == "error" { "error" } else { "ok" }
    if _strict and status != "ok" {
      panic("loom: inline " + lang + " code `" + code + "` is " + status + "; run `loom run` (strict mode)")
    }
    show: _restore.with(inline: true)
    if status == "pending" {
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
