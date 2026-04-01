/// Parsing Typst sources for Julia and R code blocks.
///
/// # Chapter discovery
///
/// The book has a two-level structure:
///
///   book.typ
///     └── #include("chapter/intro.typ")      ← chapter "intro"
///           └── #include("chapter/intro/sec2.typ")  ← same chapter
///     └── #include("chapter/ch2.typ")         ← chapter "ch2"
///     └── #include("preamble.typ")            ← special: session "preamble"
///
/// Top-level includes from book.typ define chapter boundaries.
/// A file whose stem is exactly "preamble" is tagged session = "preamble"
/// regardless of where it appears.  All other files belong to the chapter
/// determined by which top-level include brought them in.
///
/// # Cell syntax
///
///   #jlrun(id: "name", ```julia
///   code
///   ```)
///
///   #rrun(id: "name", message: false, ```r
///   code
///   ```)
///
/// An optional `session:` override lets authors force a cell into a specific
/// session (e.g. a cross-chapter shared session).
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Julia,
    R,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellKind {
    /// Silent execution.
    Run,
    /// REPL display — per-statement output capture.
    Console,
    /// Figure rendered inline from the cell's result.
    Plot,
    /// Table — captures Typst markup from the result.
    Table,
    /// Expression — captures a Typst expression (e.g. math equation) from the result.
    Expr,
    /// Code block with output — like Console but rendered without REPL prompts.
    Code,
    /// Inline expression — short code evaluated and inserted inline in text.
    Inline,
}

/// A single executable code cell.
#[derive(Debug, Clone)]
pub struct Cell {
    pub id: String,
    pub kind: CellKind,
    pub language: Language,
    pub code: String,
    /// Session name: "preamble", a chapter stem, or an explicit override.
    pub session: String,
    /// Chapter this cell belongs to (same as session unless overridden).
    pub chapter: String,
    pub source_file: PathBuf,
    pub line: usize,
    /// knitr-style options (message, warning, fig.width, etc.)
    pub options: HashMap<String, String>,
}

/// All cells grouped by chapter, in document order within each chapter.
/// The preamble chapter (if any) is always first.
#[derive(Debug, Default)]
pub struct Book {
    /// Ordered list of chapter names (preamble first if present).
    pub chapters: Vec<String>,
    /// Cells per chapter, in document order.
    pub cells: HashMap<String, Vec<Cell>>,
}

impl Book {
    pub fn all_cells(&self) -> impl Iterator<Item = &Cell> {
        self.chapters
            .iter()
            .flat_map(move |ch| self.cells.get(ch).map(|v| v.as_slice()).unwrap_or(&[]))
    }

    pub fn chapter_cells(&self, chapter: &str) -> &[Cell] {
        self.cells.get(chapter).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Whether any cell uses the given language.
    pub fn uses_language(&self, lang: Language) -> bool {
        self.all_cells().any(|c| c.language == lang)
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Parse `root` and return all cells grouped by chapter.
///
/// Works for both multi-file books (root with `#include` directives) and
/// single-file documents (all cells in the root file itself).
pub fn parse_book(root: &Path) -> Result<Book> {
    let mut book = Book::default();
    let mut visited: HashSet<PathBuf> = Default::default();

    let root_canon = root
        .canonicalize()
        .with_context(|| format!("Cannot find root file: {}", root.display()))?;
    let root_dir = root_canon.parent().unwrap_or(Path::new("."));
    let root_src = std::fs::read_to_string(&root_canon)
        .with_context(|| format!("Cannot read: {}", root_canon.display()))?;

    visited.insert(root_canon.clone());

    // Collect cells from included files (chapters).
    for included_path in parse_includes(&root_src) {
        let child = root_dir.join(&included_path);
        let child_canon = child
            .canonicalize()
            .with_context(|| format!("Cannot find included file: {}", child.display()))?;

        let chapter = file_stem(&child_canon);
        collect_chapter(child_canon, &chapter, &mut book, &mut visited)?;
    }

    // Also collect cells directly in the root file.
    // For single-file documents this is the only source of cells.
    let root_chapter = file_stem(&root_canon);
    let root_cells = parse_cells(&root_src, &root_canon, &root_chapter)?;
    if !root_cells.is_empty() {
        if !book.chapters.contains(&root_chapter) {
            // If any root cell is a preamble, insert first; otherwise append.
            if root_cells.iter().any(|c| c.session == "preamble") {
                book.chapters.insert(0, root_chapter.clone());
            } else {
                book.chapters.push(root_chapter.clone());
            }
        }
        book.cells
            .entry(root_chapter)
            .or_default()
            .extend(root_cells);
    }

    check_unique_ids(&book)?;
    Ok(book)
}

// ── Chapter collection ────────────────────────────────────────────────────────

fn collect_chapter(
    path: PathBuf,
    chapter: &str,
    book: &mut Book,
    visited: &mut HashSet<PathBuf>,
) -> Result<()> {
    if !visited.insert(path.clone()) {
        return Ok(());
    }

    let src = std::fs::read_to_string(&path)
        .with_context(|| format!("Cannot read: {}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));

    for included in parse_includes(&src) {
        let child = dir.join(&included).canonicalize().with_context(|| {
            format!(
                "Cannot find: {} (included from {})",
                included,
                path.display()
            )
        })?;
        collect_chapter(child, chapter, book, visited)?;
    }

    let cells_here = parse_cells(&src, &path, chapter)?;

    if !cells_here.is_empty() {
        if !book.chapters.contains(&chapter.to_string()) {
            if chapter == "preamble" {
                book.chapters.insert(0, chapter.to_string());
            } else {
                book.chapters.push(chapter.to_string());
            }
        }
        book.cells
            .entry(chapter.to_string())
            .or_default()
            .extend(cells_here);
    }

    Ok(())
}

// ── Cell parsing ──────────────────────────────────────────────────────────────

/// Detect the language and kind from the opening line of a cell.
fn detect_cell(line: &str) -> Option<(Language, CellKind)> {
    // Julia cells
    if line.starts_with("#jlrun(") {
        return Some((Language::Julia, CellKind::Run));
    }
    if line.starts_with("#jlconsole(") {
        return Some((Language::Julia, CellKind::Console));
    }
    if line.starts_with("#jlplot(") {
        return Some((Language::Julia, CellKind::Plot));
    }
    if line.starts_with("#jltable(") {
        return Some((Language::Julia, CellKind::Table));
    }
    if line.starts_with("#jlexpr(") {
        return Some((Language::Julia, CellKind::Expr));
    }
    if line.starts_with("#jlcode(") {
        return Some((Language::Julia, CellKind::Code));
    }
    // R cells
    if line.starts_with("#rrun(") {
        return Some((Language::R, CellKind::Run));
    }
    if line.starts_with("#rconsole(") {
        return Some((Language::R, CellKind::Console));
    }
    if line.starts_with("#rplot(") {
        return Some((Language::R, CellKind::Plot));
    }
    if line.starts_with("#rtable(") {
        return Some((Language::R, CellKind::Table));
    }
    if line.starts_with("#rexpr(") {
        return Some((Language::R, CellKind::Expr));
    }
    if line.starts_with("#rcode(") {
        return Some((Language::R, CellKind::Code));
    }
    None
}

/// knitr-style options we recognize.
const KNOWN_OPTIONS: &[&str] = &[
    "message",
    "warning",
    "error",
    "results",
    "echo",
    "eval",
    "fig-width",
    "fig-height",
    "fig_width",
    "fig_height",
    "comment",
    "collapse",
];

fn parse_options(header: &str) -> HashMap<String, String> {
    let mut opts = HashMap::new();
    for &opt in KNOWN_OPTIONS {
        // Match: option: value  (value can be bool, number, or quoted string)
        let pattern = format!(
            r#"(?:^|[,\s]){}:\s*(?:"([^"]+)"|([^\s,`]+))"#,
            regex::escape(opt)
        );
        if let Ok(re) = regex::Regex::new(&pattern) {
            if let Some(caps) = re.captures(header) {
                let val = caps.get(1).or(caps.get(2)).map(|m| m.as_str().to_string());
                if let Some(v) = val {
                    opts.insert(opt.to_string(), v);
                }
            }
        }
    }
    opts
}

fn parse_cells(src: &str, path: &Path, default_chapter: &str) -> Result<Vec<Cell>> {
    let lines: Vec<&str> = src.lines().collect();
    let mut cells = Vec::new();
    let mut i = 0;

    // Inline code patterns: #ri("...") and #jli("...")
    let ri_re = regex::Regex::new(r#"#ri\("([^"]*)"\)"#).unwrap();
    let jli_re = regex::Regex::new(r#"#jli\("([^"]*)"\)"#).unwrap();
    let mut seen_inline: std::collections::HashSet<String> = std::collections::HashSet::new();

    while i < lines.len() {
        let line = lines[i].trim_start();

        let detected = detect_cell(line);

        if let Some((language, kind)) = detected {
            let line_no = i + 1;
            let lang_name = match language {
                Language::Julia => "Julia",
                Language::R => "R",
            };

            // Accumulate lines until we find the opening fence.
            let mut header = line.to_string();
            while !header.contains("```") {
                i += 1;
                if i >= lines.len() {
                    bail!(
                        "{}:{}: expected opening ``` for {} block",
                        path.display(),
                        line_no,
                        lang_name
                    );
                }
                header.push(' ');
                header.push_str(lines[i].trim());
            }

            let id = extract_attr(&header, "id", path, line_no)?;

            let session =
                extract_attr_opt(&header, "session").unwrap_or_else(|| default_chapter.to_string());

            let options = parse_options(&header);

            // Collect code lines until closing fence.
            i += 1;
            let mut code_lines: Vec<&str> = Vec::new();
            loop {
                if i >= lines.len() {
                    bail!(
                        "{}:{}: unterminated {} block",
                        path.display(),
                        line_no,
                        lang_name
                    );
                }
                let trimmed = lines[i].trim();
                if trimmed == "```)" || trimmed == "```" {
                    break;
                }
                code_lines.push(lines[i]);
                i += 1;
            }

            cells.push(Cell {
                id,
                kind,
                language,
                code: code_lines.join("\n"),
                session: session.clone(),
                chapter: default_chapter.to_string(),
                source_file: path.to_owned(),
                line: line_no,
                options,
            });
        } else {
            // Scan for inline code: #ri("...") and #jli("...")
            let raw_line = lines[i];
            for cap in ri_re.captures_iter(raw_line) {
                let code = cap[1].to_string();
                let id = format!("_ri:{}", code);
                if seen_inline.insert(id.clone()) {
                    cells.push(Cell {
                        id,
                        kind: CellKind::Inline,
                        language: Language::R,
                        code,
                        session: default_chapter.to_string(),
                        chapter: default_chapter.to_string(),
                        source_file: path.to_owned(),
                        line: i + 1,
                        options: HashMap::new(),
                    });
                }
            }
            for cap in jli_re.captures_iter(raw_line) {
                let code = cap[1].to_string();
                let id = format!("_jli:{}", code);
                if seen_inline.insert(id.clone()) {
                    cells.push(Cell {
                        id,
                        kind: CellKind::Inline,
                        language: Language::Julia,
                        code,
                        session: default_chapter.to_string(),
                        chapter: default_chapter.to_string(),
                        source_file: path.to_owned(),
                        line: i + 1,
                        options: HashMap::new(),
                    });
                }
            }
        }

        i += 1;
    }

    Ok(cells)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_includes(src: &str) -> Vec<String> {
    let re = regex::Regex::new(r#"#include\s*(?:\(\s*"([^"]+)"\s*\)|"([^"]+)")"#).unwrap();
    // Only match non-commented lines.
    src.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .flat_map(|line| {
            re.captures_iter(line)
                .map(|c| c.get(1).or(c.get(2)).unwrap().as_str().to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn extract_attr(line: &str, attr: &str, path: &Path, line_no: usize) -> Result<String> {
    extract_attr_opt(line, attr).with_context(|| {
        format!(
            "{}:{}: missing {}:\"...\" in code block",
            path.display(),
            line_no,
            attr
        )
    })
}

fn extract_attr_opt(line: &str, attr: &str) -> Option<String> {
    let pattern = format!(r#"{}\s*:\s*"([^"]+)""#, regex::escape(attr));
    let re = regex::Regex::new(&pattern).unwrap();
    re.captures(line).map(|c| c[1].to_string())
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn check_unique_ids(book: &Book) -> Result<()> {
    let mut seen: HashMap<&str, (&Cell, &str)> = Default::default();
    for (ch, cells) in &book.cells {
        for cell in cells {
            if let Some((prev, prev_ch)) = seen.insert(&cell.id, (cell, ch)) {
                bail!(
                    "Duplicate cell id '{}': \
                     first in chapter '{}' at {}:{}, \
                     redefined in chapter '{}' at {}:{}",
                    cell.id,
                    prev_ch,
                    prev.source_file.display(),
                    prev.line,
                    ch,
                    cell.source_file.display(),
                    cell.line,
                );
            }
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_julia_cell() {
        let src = "#jlrun(id: \"setup\", ```\nx = 1\n```)";
        let path = PathBuf::from("test.typ");
        let cells = parse_cells(src, &path, "intro").unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].id, "setup");
        assert_eq!(cells[0].language, Language::Julia);
        assert_eq!(cells[0].session, "intro");
        assert_eq!(cells[0].code, "x = 1");
    }

    #[test]
    fn test_parse_r_cell() {
        let src = "#rrun(id: \"setup\", message: false, ```r\nx <- 1\n```)";
        let path = PathBuf::from("test.typ");
        let cells = parse_cells(src, &path, "intro").unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].id, "setup");
        assert_eq!(cells[0].language, Language::R);
        assert_eq!(
            cells[0].options.get("message").map(|s| s.as_str()),
            Some("false")
        );
    }

    #[test]
    fn test_session_override() {
        let src = "#jlrun(id: \"shared\", session: \"preamble\", ```\nusing Foo\n```)";
        let path = PathBuf::from("ch1.typ");
        let cells = parse_cells(src, &path, "ch1").unwrap();
        assert_eq!(cells[0].session, "preamble");
        assert_eq!(cells[0].chapter, "ch1");
    }

    #[test]
    fn test_parse_plot_cells() {
        let src = "#jlplot(id: \"fig\", ```julia\nplot(1:3)\n```)\n#rplot(id: \"rfig\", ```r\nplot(1:3)\n```)";
        let path = PathBuf::from("plots.typ");
        let cells = parse_cells(src, &path, "plots").unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].kind, CellKind::Plot);
        assert_eq!(cells[0].language, Language::Julia);
        assert_eq!(cells[1].kind, CellKind::Plot);
        assert_eq!(cells[1].language, Language::R);
    }

    #[test]
    fn test_parse_includes() {
        let src = r#"#include("chapter/intro.typ")
#include("chapter/ch2.typ")"#;
        let includes = parse_includes(src);
        assert_eq!(includes, vec!["chapter/intro.typ", "chapter/ch2.typ"]);

        let src2 = r#"#include "chapter/intro.typ"
#include "chapter/ch2.typ""#;
        let includes2 = parse_includes(src2);
        assert_eq!(includes2, vec!["chapter/intro.typ", "chapter/ch2.typ"]);
    }
}
