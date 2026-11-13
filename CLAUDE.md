# loom: notes for working on the code

loom executes Julia, R, and Python cells written as raw blocks in Typst
documents. User documentation is in `README.md`; this file describes the
implementation.

## Layout

| Path | Role |
|---|---|
| `src/syntax.rs` | finds cells with `typst-syntax`; chapters from `#include` |
| `src/options.rs` | `#| key: value` options; exec vs. render options |
| `src/plan.rs` | sessions and chained cache keys |
| `src/store.rs` | result store, content-addressed figures, `cells.json` |
| `src/kernel.rs` | kernel processes, protocol client, project lock |
| `src/runner.rs` | decides what to execute (resume or reset) |
| `src/main.rs` | CLI and `watch` loop |
| `kernels/PROTOCOL.md` | contract between `loom` and the kernels |
| `kernels/{julia.jl,r.R,python.py}` | kernels, embedded in the binary |
| `typst/loom.typ` | Typst library, written to `.loom/loom.typ` |
| `templates/loom.toml` | template written by `loom init` |

## Invariants

- **Lookup keys.** The Typst library looks results up by
  `"b:" | "i:"` + language + `:` + code, where code is the raw text without
  its leading `#|` lines. `src/options.rs::split_options` and
  `typst/loom.typ::_split` must implement the same rule, and the parser must
  produce exactly Typst's `raw.text` (it uses the `typst-syntax` version that
  matches the supported Typst). `src/typst_tests.rs` checks this by compiling
  in strict mode against every parsed key.
- **Cache keys** chain through a session (`plan.rs`). A stored result is
  never invalidated in place; edits produce new keys.
- **Exec options** (`options::EXEC_KEYS`, mirrored in `_exec-keys` in the
  Typst library) enter the cache key; render options must not.
- **Positions.** A kernel executes a request only if the session's position
  equals `after`; the runner resumes a session only when the kernel's position
  equals the key before the first stale cell.
- **Kernels** use only their language's standard library plus, for R,
  `jsonlite` and `evaluate`. They listen on 127.0.0.1, check the token, and
  exit when the parent `loom` process disappears.
- Writes that Tinymist may read (`cells.json`, figures) are atomic.

## Testing

```sh
cargo test                       # unit tests; Typst tests run if `typst` is on PATH
```

The kernels are tested through the protocol by small throwaway clients;
end-to-end checks run `loom run` and `typst compile --input loom=strict` on a
scratch document.
