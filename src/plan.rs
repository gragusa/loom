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
//! that produced it.

use crate::config::{ExecuteDefaults, FigFormat};
use crate::options::SETUP_SESSION;
use crate::syntax::{Cell, Document, Lang};
use serde_json::json;
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
        })
    }

    fn canonical(&self) -> String {
        format!(
            "fig-width={};fig-height={};fig-format={};fig-dpi={};error={}",
            self.fig_width,
            self.fig_height,
            self.fig_format.as_str(),
            self.fig_dpi,
            self.error
        )
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
}

impl SessionPlan {
    pub fn is_setup(&self) -> bool {
        self.name == SETUP_SESSION
    }

    /// Key of the session state in which cell `i` (an index into `cells`)
    /// runs.
    pub fn key_before(&self, i: usize) -> &str {
        if i == 0 { &self.key0 } else { &self.keys[i - 1] }
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

pub fn build_plan(doc: &Document, defaults: &ExecuteDefaults) -> Plan {
    // Group cells into sessions, preserving first-appearance order.
    let mut sessions: Vec<SessionPlan> = Vec::new();
    let mut exec = Vec::with_capacity(doc.cells.len());
    for (i, cell) in doc.cells.iter().enumerate() {
        if !cell.options.eval {
            exec.push(None);
            continue;
        }
        exec.push(Some(ExecOptions::resolve(cell, defaults)));
        let name = cell.session();
        match sessions
            .iter_mut()
            .find(|s| s.lang == cell.lang && s.name == name)
        {
            Some(s) => s.cells.push(i),
            None => sessions.push(SessionPlan {
                lang: cell.lang,
                name: name.to_string(),
                key0: String::new(),
                cells: vec![i],
                keys: Vec::new(),
            }),
        }
    }
    // Setup sessions run first: every other session's first key depends on
    // the last key of its language's setup session.
    sessions.sort_by_key(|s| !s.is_setup());

    let mut cell_keys = vec![None; doc.cells.len()];
    let mut setup_tail: Vec<(Lang, String)> = Vec::new();
    for s in &mut sessions {
        let tail = setup_tail
            .iter()
            .find(|(l, _)| *l == s.lang)
            .map(|(_, k)| k.as_str())
            .unwrap_or("");
        s.key0 = hash_parts(&[CACHE_VERSION, s.lang.tag(), &s.name, tail]);
        let mut prev = s.key0.clone();
        for &i in &s.cells {
            let cell = &doc.cells[i];
            let opts = exec[i].as_ref().expect("evaluated cell has options");
            let raw: String = cell
                .options
                .exec_raw()
                .iter()
                .map(|(k, v)| format!("{k}={v};"))
                .collect();
            let mode = if cell.inline { "inline" } else { "block" };
            let key = hash_parts(&[&prev, mode, &cell.code, &opts.canonical(), &raw]);
            s.keys.push(key.clone());
            cell_keys[i] = Some(key.clone());
            prev = key;
        }
        if s.is_setup() {
            setup_tail.push((s.lang, prev));
        }
    }
    Plan {
        sessions,
        cell_keys,
        exec,
    }
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
        let plan = build_plan(&doc, &ExecuteDefaults::default());
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
}
