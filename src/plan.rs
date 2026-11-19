//! Sessions and cache keys.
//!
//! A session is the ordered list of cells, of one language, that share a
//! namespace: by default the cells of one chapter, or the cells that name the
//! same `#| session:`. Setup cells of a language form the session `setup`,
//! whose code is also replayed at the start of every other session of that
//! language.
//!
//! Cache keys are chained: the key of a cell hashes the key of the previous
//! cell of its session together with the cell's code and execution options,
//! and the first key of a session hashes the last key of the setup session.
//! A stored result is therefore valid only for the exact sequence of code
//! that produced it. Cells marked `stateless` do not change the session, so
//! they are left out of the chain: editing one invalidates only itself.

use crate::config::{ExecuteDefaults, FigFormat};
use crate::options::SETUP_SESSION;
use crate::syntax::{Cell, Document, Lang};
use anyhow::{Result, bail};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use sha2::{Digest, Sha256};

/// Changing this string invalidates every stored result; it must change
/// whenever the meaning or format of stored outputs changes.
pub const CACHE_VERSION: &str = "loom-cache-1";

/// Execution options of a cell after applying `loom.toml` defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecOptions {
    pub fig_width: f64,
    pub fig_height: f64,
    pub fig_format: FigFormat,
    pub fig_dpi: f64,
    /// Characters per line of printed values and text output.
    pub print_width: u32,
    /// An error is expected output rather than a failure.
    pub error: bool,
}

impl ExecOptions {
    pub fn resolve(cell: &Cell, d: &ExecuteDefaults) -> Self {
        let o = &cell.options;
        Self {
            fig_width: o.fig_width.unwrap_or(d.fig_width),
            fig_height: o.fig_height.unwrap_or(d.fig_height),
            fig_format: o.fig_format.unwrap_or(d.fig_format),
            fig_dpi: o.fig_dpi.unwrap_or(d.fig_dpi),
            print_width: o.print_width.unwrap_or(d.print_width),
            error: o.error.unwrap_or(d.error),
        }
    }

    /// The `options` object of an `exec` request.
    pub fn kernel_options(&self) -> serde_json::Value {
        json!({
            "fig-width": self.fig_width,
            "fig-height": self.fig_height,
            "fig-format": self.fig_format.as_str(),
            "fig-dpi": self.fig_dpi,
            "print-width": self.print_width,
        })
    }

    fn canonical(&self) -> String {
        let mut s = format!(
            "fig-width={};fig-height={};fig-format={};fig-dpi={};error={}",
            self.fig_width,
            self.fig_height,
            self.fig_format.as_str(),
            self.fig_dpi,
            self.error
        );
        // The default width adds nothing, so keys computed before the option
        // existed still match the results they name.
        if self.print_width != 80 {
            s.push_str(&format!(";print-width={}", self.print_width));
        }
        s
    }
}

#[derive(Debug, Clone)]
pub struct SessionPlan {
    pub lang: Lang,
    pub name: String,
    /// Key of the empty session (after setup code has been replayed).
    pub key0: String,
    /// Indices into `Document::cells`, in document order.
    pub cells: Vec<usize>,
    /// Cache key of each cell in `cells`.
    pub keys: Vec<String>,
    /// Key of the session state in which each cell runs: the key of the
    /// last earlier cell that is not stateless, or `key0`.
    pub before: Vec<String>,
    /// Whether each cell is stateless.
    pub stateless: Vec<bool>,
}

impl SessionPlan {
    pub fn is_setup(&self) -> bool {
        self.name == SETUP_SESSION
    }

    /// Key of the session state in which cell `i` (an index into `cells`)
    /// runs.
    pub fn key_before(&self, i: usize) -> &str {
        &self.before[i]
    }

    /// Key of the session state after cell `i`: a stateless cell leaves it
    /// unchanged.
    pub fn key_after(&self, i: usize) -> &str {
        if self.stateless[i] { &self.before[i] } else { &self.keys[i] }
    }
}

#[derive(Debug, Default)]
pub struct Plan {
    /// Setup sessions first, then the others in order of first appearance.
    pub sessions: Vec<SessionPlan>,
    /// Cache key of each cell; `None` for cells with `eval: false`.
    pub cell_keys: Vec<Option<String>>,
    /// Effective execution options of each cell; `None` for `eval: false`.
    pub exec: Vec<Option<ExecOptions>>,
    /// For each cell, its imports and the index of the cell whose export
    /// each one reads: the latest earlier cell exporting that name.
    pub imports: Vec<Vec<(String, usize)>>,
    /// Token of the cell the Typst library will find before each cell in
    /// its session (see `typst_predecessors`).
    pub after: Vec<Option<String>>,
    /// For each cell, the token of the exporting cell of each import.
    pub import_tokens: Vec<BTreeMap<String, String>>,
}

impl Plan {
    /// Code replayed at the start of every non-setup session of `lang`.
    pub fn setup_code(&self, doc: &Document, lang: Lang) -> Vec<String> {
        self.sessions
            .iter()
            .filter(|s| s.lang == lang && s.is_setup())
            .flat_map(|s| s.cells.iter().map(|&i| doc.cells[i].code.clone()))
            .collect()
    }
}

pub fn build_plan(doc: &Document, defaults: &ExecuteDefaults) -> Result<Plan> {
    // Group cells into sessions, preserving first-appearance order.
    let mut sessions: Vec<SessionPlan> = Vec::new();
    let mut session_of = vec![usize::MAX; doc.cells.len()];
    let mut exec = Vec::with_capacity(doc.cells.len());
    for (i, cell) in doc.cells.iter().enumerate() {
        if !cell.options.eval {
            exec.push(None);
            continue;
        }
        exec.push(Some(ExecOptions::resolve(cell, defaults)));
        let name = cell.session();
        let si = match sessions
            .iter()
            .position(|s| s.lang == cell.lang && s.name == name)
        {
            Some(si) => si,
            None => {
                sessions.push(SessionPlan {
                    lang: cell.lang,
                    name: name.to_string(),
                    key0: String::new(),
                    cells: Vec::new(),
                    keys: Vec::new(),
                    before: Vec::new(),
                    stateless: Vec::new(),
                });
                sessions.len() - 1
            }
        };
        sessions[si].cells.push(i);
        session_of[i] = si;
    }

    let imports = resolve_imports(doc)?;

    // Setup sessions are keyed first: every other session's first key
    // depends on the last key of its language's setup session. Setup cells
    // cannot import, so they depend on nothing else.
    let mut cell_keys: Vec<Option<String>> = vec![None; doc.cells.len()];
    let mut prev: Vec<String> = vec![String::new(); sessions.len()];
    let mut setup_tail: Vec<(Lang, String)> = Vec::new();
    for si in 0..sessions.len() {
        if !sessions[si].is_setup() {
            continue;
        }
        let s = &mut sessions[si];
        s.key0 = hash_parts(&[CACHE_VERSION, s.lang.tag(), &s.name, ""]);
        prev[si] = s.key0.clone();
        for &i in &s.cells.clone() {
            let key = cell_key(doc, &exec, &cell_keys, &imports, i, &prev[si]);
            push_key(s, &mut prev[si], key.clone(), doc.cells[i].is_stateless());
            cell_keys[i] = Some(key);
        }
        setup_tail.push((s.lang, prev[si].clone()));
    }
    // Other cells in document order, so that the cell an import reads from,
    // which comes earlier in the document, is keyed before the cell reading.
    for i in 0..doc.cells.len() {
        let si = session_of[i];
        if si == usize::MAX || sessions[si].is_setup() {
            continue;
        }
        let s = &mut sessions[si];
        if s.keys.is_empty() {
            let tail = setup_tail
                .iter()
                .find(|(l, _)| *l == s.lang)
                .map(|(_, k)| k.as_str())
                .unwrap_or("");
            s.key0 = hash_parts(&[CACHE_VERSION, s.lang.tag(), &s.name, tail]);
            prev[si] = s.key0.clone();
        }
        let key = cell_key(doc, &exec, &cell_keys, &imports, i, &prev[si]);
        push_key(s, &mut prev[si], key.clone(), doc.cells[i].is_stateless());
        cell_keys[i] = Some(key);
    }
    // Setup sessions run first.
    sessions.sort_by_key(|s| !s.is_setup());

    let (after, import_tokens) = typst_predecessors(doc);
    Ok(Plan {
        sessions,
        cell_keys,
        exec,
        imports,
        after,
        import_tokens,
    })
}

fn cell_key(
    doc: &Document,
    exec: &[Option<ExecOptions>],
    cell_keys: &[Option<String>],
    imports: &[Vec<(String, usize)>],
    i: usize,
    prev: &str,
) -> String {
    let cell = &doc.cells[i];
    let opts = exec[i].as_ref().expect("evaluated cell has options");
    let raw: String = cell
        .options
        .exec_raw()
        .iter()
        .map(|(k, v)| format!("{k}={v};"))
        .collect();
    // A cell that imports depends on the exact result it reads from.
    let sources: String = imports[i]
        .iter()
        .map(|(name, e)| {
            format!("{name}={};", cell_keys[*e].as_deref().expect("exporter keyed first"))
        })
        .collect();
    let mode = if cell.inline { "inline" } else { "block" };
    hash_parts(&[prev, mode, &cell.code, &opts.canonical(), &raw, &sources])
}

fn push_key(s: &mut SessionPlan, prev: &mut String, key: String, stateless: bool) {
    s.before.push(prev.clone());
    s.stateless.push(stateless);
    s.keys.push(key.clone());
    if !stateless {
        *prev = key;
    }
}

/// For each cell, the cells its imports read from: for every imported name,
/// the latest earlier evaluated cell that exports it.
fn resolve_imports(doc: &Document) -> Result<Vec<Vec<(String, usize)>>> {
    let mut latest: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::with_capacity(doc.cells.len());
    for (i, cell) in doc.cells.iter().enumerate() {
        let mut here = Vec::new();
        if cell.options.eval {
            for name in &cell.options.imports {
                let Some(&e) = latest.get(name.as_str()) else {
                    bail!(
                        "the {} cell at {}:{} imports `{name}`, but no earlier cell exports it",
                        cell.lang.name(),
                        cell.file.display(),
                        cell.line
                    );
                };
                here.push((name.clone(), e));
            }
            for name in &cell.options.exports {
                latest.insert(name, i);
            }
        }
        out.push(here);
    }
    Ok(out)
}

/// The document-order bookkeeping of the Typst library, run over the
/// unchanged document. Each evaluated cell has a token (its lookup key and
/// occurrence number); the library remembers, per session, the token of the
/// last cell that is not stateless, falling back to the language's setup
/// session for a session's first cell, and, per exported name, the token of
/// the latest cell exporting it. A stored result records the tokens expected
/// before its cell and for its imports, so the library can tell when an
/// earlier cell of the session, or a cell it imports from, was edited,
/// inserted, or removed. The library's `_advance` must follow the same rules.
fn typst_predecessors(doc: &Document) -> (Vec<Option<String>>, Vec<BTreeMap<String, String>>) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut last: HashMap<String, String> = HashMap::new();
    let mut exported: HashMap<String, String> = HashMap::new();
    let mut after = Vec::with_capacity(doc.cells.len());
    let mut import_tokens = Vec::with_capacity(doc.cells.len());
    for cell in &doc.cells {
        if !cell.options.eval {
            after.push(None);
            import_tokens.push(BTreeMap::new());
            continue;
        }
        let key = cell.lookup_key();
        let n = counts.entry(key.clone()).or_insert(0);
        *n += 1;
        let token = format!("{key}\u{1}{n}");
        let session = format!("{}/{}", cell.lang.tag(), cell.session());
        let setup = format!("{}/{SETUP_SESSION}", cell.lang.tag());
        let prev = last.get(&session).or_else(|| last.get(&setup)).cloned();
        let imports: BTreeMap<String, String> = cell
            .options
            .imports
            .iter()
            .filter_map(|name| exported.get(name).map(|t| (name.clone(), t.clone())))
            .collect();
        if !cell.is_stateless() {
            last.insert(session, token.clone());
        }
        for name in &cell.options.exports {
            exported.insert(name.clone(), token.clone());
        }
        after.push(prev);
        import_tokens.push(imports);
    }
    (after, import_tokens)
}

/// SHA-256 of length-prefixed parts, truncated to 128 bits (32 hex digits).
fn hash_parts(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
    }
    hex::encode(&h.finalize()[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Project;
    use crate::syntax::parse_document;

    fn plan_for(files: &[(&str, &str)]) -> (Document, Plan) {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            let p = dir.path().join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        let project = Project::from_root(&dir.path().join(files[0].0)).unwrap();
        let doc = parse_document(&project).unwrap();
        let plan = build_plan(&doc, &ExecuteDefaults::default()).unwrap();
        (doc, plan)
    }

    fn keys(files: &[(&str, &str)]) -> Vec<Option<String>> {
        plan_for(files).1.cell_keys
    }

    const BOOK: &str = "#include \"preamble.typ\"\n#include \"a.typ\"\n#include \"b.typ\"\n";

    #[test]
    fn sessions_follow_chapters_and_overrides() {
        let (_, plan) = plan_for(&[
            ("book.typ", BOOK),
            ("preamble.typ", "```julia\nusing X\n```\n```r\nlibrary(y)\n```\n"),
            ("a.typ", "```julia\na = 1\n```\n```julia\n#| session: shared\ns = 1\n```\n"),
            ("b.typ", "```julia\nb = 1\n```\n```julia\n#| session: shared\ns + 1\n```\n```julia\n#| eval: false\nnot run\n```\n"),
        ]);
        let names: Vec<_> = plan
            .sessions
            .iter()
            .map(|s| (s.lang, s.name.as_str(), s.cells.clone()))
            .collect();
        assert_eq!(
            names,
            vec![
                (Lang::Julia, "setup", vec![0]),
                (Lang::R, "setup", vec![1]),
                (Lang::Julia, "a", vec![2]),
                (Lang::Julia, "shared", vec![3, 5]),
                (Lang::Julia, "b", vec![4]),
            ]
        );
        assert!(plan.cell_keys[6].is_none());
    }

    #[test]
    fn editing_a_cell_changes_its_key_and_later_keys_only() {
        let before = keys(&[("doc.typ", "```julia\na = 1\n```\n```julia\nb = 2\n```\n```julia\nc = 3\n```\n")]);
        let after = keys(&[("doc.typ", "```julia\na = 1\n```\n```julia\nb = 20\n```\n```julia\nc = 3\n```\n")]);
        assert_eq!(before[0], after[0]);
        assert_ne!(before[1], after[1]);
        assert_ne!(before[2], after[2]);
    }

    #[test]
    fn render_options_do_not_change_keys_but_exec_options_do() {
        let base = keys(&[("doc.typ", "```julia\nx = 1\n```\n")]);
        let render = keys(&[("doc.typ", "```julia\n#| echo: false\n#| fig-cap: A cap.\nx = 1\n```\n")]);
        let exec = keys(&[("doc.typ", "```julia\n#| fig-width: 3\nx = 1\n```\n")]);
        assert_eq!(base[0], render[0]);
        assert_ne!(base[0], exec[0]);
    }

    #[test]
    fn imports_depend_on_the_exporting_cell() {
        let doc = |r: &str| {
            format!(
                "```r\n#| export: df\n{r}\n```\n```python\nx = 1\n```\n```python\n#| import: df\ndf.shape\n```\n```julia\ny = 2\n```\n"
            )
        };
        let (_, plan) = plan_for(&[("doc.typ", &doc("df <- mtcars"))]);
        assert_eq!(plan.imports[2], vec![("df".to_string(), 0)]);
        let k1 = plan.cell_keys.clone();
        let (_, plan2) = plan_for(&[("doc.typ", &doc("df <- iris"))]);
        let k2 = plan2.cell_keys;
        assert_ne!(k1[0], k2[0]);
        assert_eq!(k1[1], k2[1], "a Python cell that does not import is unaffected");
        assert_ne!(k1[2], k2[2], "the importing cell follows its exporter");
        assert_eq!(k1[3], k2[3]);
    }

    #[test]
    fn an_import_without_an_earlier_export_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doc.typ");
        std::fs::write(&p, "```python\n#| import: df\nx\n```\n```r\n#| export: df\ndf <- 1\n```\n").unwrap();
        let project = Project::from_root(&p).unwrap();
        let doc = parse_document(&project).unwrap();
        let err = build_plan(&doc, &ExecuteDefaults::default()).unwrap_err().to_string();
        assert!(err.contains("imports `df`, but no earlier cell exports it"), "{err}");
    }

    #[test]
    fn editing_a_stateless_cell_changes_only_its_key() {
        let doc = |b: &str| {
            format!("```julia\na = 1\n```\n```julia\n#| stateless: true\n{b}\n```\n```julia\nc = 3\n```\n")
        };
        let k1 = keys(&[("doc.typ", &doc("show(a)"))]);
        let k2 = keys(&[("doc.typ", &doc("show(a + 1)"))]);
        assert_eq!(k1[0], k2[0]);
        assert_ne!(k1[1], k2[1]);
        assert_eq!(k1[2], k2[2]);
    }

    #[test]
    fn setup_changes_invalidate_every_session_of_that_language() {
        let files = |setup: &'static str| {
            vec![
                ("book.typ", BOOK),
                ("preamble.typ", setup),
                ("a.typ", "```julia\na = 1\n```\n```r\nx <- 1\n```\n"),
                ("b.typ", "```julia\nb = 1\n```\n"),
            ]
        };
        let k1 = keys(&files("```julia\nusing X\n```\n"));
        let k2 = keys(&files("```julia\nusing X, Y\n```\n"));
        // Julia cells in both chapters change; the R cell does not.
        assert_ne!(k1[1], k2[1]);
        assert_ne!(k1[3], k2[3]);
        assert_eq!(k1[2], k2[2]);
    }

    #[test]
    fn identical_code_in_different_sessions_gets_different_keys() {
        let k = keys(&[
            ("book.typ", "#include \"a.typ\"\n#include \"b.typ\"\n"),
            ("a.typ", "```julia\nx\n```\n"),
            ("b.typ", "```julia\nx\n```\n"),
        ]);
        assert_ne!(k[0], k[1]);
    }

    #[test]
    fn default_print_width_leaves_existing_keys_unchanged() {
        let (doc, _) = plan_for(&[("doc.typ", "```julia\nx = 1\n```\n")]);
        let resolved = ExecOptions::resolve(&doc.cells[0], &ExecuteDefaults::default());
        assert_eq!(resolved.print_width, 80);
        let canonical = |w| ExecOptions { print_width: w, ..resolved.clone() }.canonical();
        assert!(!canonical(80).contains("print-width"));
        assert!(canonical(64).ends_with(";print-width=64"));
        let base = keys(&[("doc.typ", "```julia\nx = 1\n```\n")]);
        let narrow = keys(&[("doc.typ", "```julia\n#| print-width: 64\nx = 1\n```\n")]);
        assert_ne!(base[0], narrow[0]);
    }
}
