//! Finding code cells in Typst sources.
//!
//! A cell is a fenced raw block whose language tag is `julia`, `r`, or
//! `python`, or an inline raw element whose text starts with `{julia}`,
//! `{r}`, or `{python}` followed by whitespace. Sources are parsed with
//! `typst-syntax`, the compiler's own parser, so comments, code mode, and
//! nested markup are handled exactly as Typst handles them.
//!
//! Chapters: each `#include` in the root file starts a chapter named after
//! the included file's stem; files included from a chapter belong to it.
//! Cells in the root file form a chapter named after the root file's stem.
//! Cells in a file whose stem is `preamble` are setup cells unless they say
//! `#| setup: false`.

use crate::config::Project;
use crate::options::{self, CellOptions, SETUP_SESSION};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use typst_syntax::{LinkedNode, SyntaxKind, ast};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lang {
    Julia,
    R,
    Python,
}

impl Lang {
    pub const ALL: [Lang; 3] = [Lang::Julia, Lang::R, Lang::Python];

    /// The raw-block language tag, also used in cell lookup keys.
    pub fn tag(self) -> &'static str {
        match self {
            Lang::Julia => "julia",
            Lang::R => "r",
            Lang::Python => "python",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.tag() == tag)
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Julia => "Julia",
            Lang::R => "R",
            Lang::Python => "Python",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Cell {
    pub lang: Lang,
    pub inline: bool,
    /// Code without option lines; this is what the kernel executes.
    pub code: String,
    pub options: CellOptions,
    pub file: PathBuf,
    /// 1-based line of the cell's opening backticks.
    pub line: usize,
    /// 1-based line, in `file`, of the first line of `code`.
    pub code_line: usize,
    pub chapter: String,
}

impl Cell {
    /// Key under which the Typst library looks up the cell's results. The
    /// library computes the same string from the raw element it is showing.
    pub fn lookup_key(&self) -> String {
        let kind = if self.inline { "i" } else { "b" };
        format!("{kind}:{}:{}", self.lang.tag(), self.code)
    }

    pub fn is_setup(&self) -> bool {
        self.options.setup == Some(true)
    }

    pub fn session(&self) -> &str {
        if self.is_setup() {
            SETUP_SESSION
        } else {
            self.options.session.as_deref().unwrap_or(&self.chapter)
        }
    }

    pub fn location(&self, project: &Project) -> String {
        format!("{}:{}", project.display_path(&self.file), self.line)
    }
}

#[derive(Debug, Default)]
pub struct Document {
    /// Chapter names in document order.
    pub chapters: Vec<String>,
    /// Cells in document order.
    pub cells: Vec<Cell>,
    /// Every source file read, in document order.
    pub files: Vec<PathBuf>,
}

pub fn parse_document(project: &Project) -> Result<Document> {
    let mut walker = Walker {
        project,
        doc: Document::default(),
        stack: Vec::new(),
    };
    let root_chapter = file_stem(&project.root_file);
    walker.walk_file(&project.root_file, None, &root_chapter)?;
    check_unique_labels(project, &walker.doc)?;
    Ok(walker.doc)
}

fn check_unique_labels(project: &Project, doc: &Document) -> Result<()> {
    let mut seen: std::collections::HashMap<&str, &Cell> = Default::default();
    for cell in &doc.cells {
        let Some(label) = cell.options.raw.get("label") else {
            continue;
        };
        let label = label.trim_matches(|c| c == '"' || c == '\'');
        if let Some(first) = seen.insert(label, cell) {
            bail!(
                "label `{label}` is used by the cells at {} and {}",
                first.location(project),
                cell.location(project)
            );
        }
    }
    Ok(())
}

struct Walker<'a> {
    project: &'a Project,
    doc: Document,
    /// Files currently being walked, to report include cycles.
    stack: Vec<PathBuf>,
}

struct FileCtx<'a> {
    path: &'a Path,
    line_starts: Vec<usize>,
    /// `None` while walking the root file itself.
    chapter: Option<&'a str>,
    root_chapter: &'a str,
    preamble: bool,
}

impl FileCtx<'_> {
    fn line_of(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }
}

impl<'a> Walker<'a> {
    fn walk_file(&mut self, path: &Path, chapter: Option<&str>, root_chapter: &str) -> Result<()> {
        if self.stack.iter().any(|p| p == path) {
            bail!(
                "cyclic #include of {}",
                self.project.display_path(path)
            );
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        self.stack.push(path.to_path_buf());
        self.doc.files.push(path.to_path_buf());
        if let Some(ch) = chapter {
            self.add_chapter(ch);
        }
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        let ctx = FileCtx {
            path,
            line_starts,
            chapter,
            root_chapter,
            preamble: file_stem(path) == "preamble",
        };
        let root = typst_syntax::parse(&text);
        self.visit(LinkedNode::new(&root), &ctx)?;
        self.stack.pop();
        Ok(())
    }

    fn add_chapter(&mut self, name: &str) {
        if !self.doc.chapters.iter().any(|c| c == name) {
            self.doc.chapters.push(name.to_string());
        }
    }

    fn visit(&mut self, node: LinkedNode, ctx: &FileCtx) -> Result<()> {
        match node.kind() {
            SyntaxKind::Raw => self.raw(&node, ctx),
            SyntaxKind::ModuleInclude => {
                self.include(&node, ctx)?;
                // The include's argument may itself contain markup.
                for child in node.children() {
                    self.visit(child, ctx)?;
                }
                Ok(())
            }
            _ => {
                for child in node.children() {
                    self.visit(child, ctx)?;
                }
                Ok(())
            }
        }
    }

    fn include(&mut self, node: &LinkedNode, ctx: &FileCtx) -> Result<()> {
        let include: ast::ModuleInclude = node.cast().expect("ModuleInclude node");
        let line = ctx.line_of(node.offset());
        let ast::Expr::Str(s) = include.source() else {
            log::warn!(
                "{}:{}: #include with a computed path; cells in that file are not found",
                self.project.display_path(ctx.path),
                line
            );
            return Ok(());
        };
        let target = s.get();
        let resolved = if let Some(rel) = target.strip_prefix('/') {
            self.project.dir.join(rel)
        } else {
            ctx.path.parent().unwrap_or(Path::new(".")).join(target.as_str())
        };
        let resolved = resolved.canonicalize().with_context(|| {
            format!(
                "{}:{}: cannot find included file {}",
                self.project.display_path(ctx.path),
                line,
                target
            )
        })?;
        let chapter = match ctx.chapter {
            Some(ch) => ch.to_string(),
            None => file_stem(&resolved),
        };
        self.walk_file(&resolved, Some(&chapter), ctx.root_chapter)
    }

    fn raw(&mut self, node: &LinkedNode, ctx: &FileCtx) -> Result<()> {
        let raw: ast::Raw = node.cast().expect("Raw node");
        let text = raw
            .lines()
            .map(|l| l.get().as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let line = ctx.line_of(node.offset());
        let first_text_line = node
            .children()
            .find(|c| c.kind() == SyntaxKind::Text)
            .map(|c| ctx.line_of(c.offset()))
            .unwrap_or(line);

        let (lang, inline, code, options) = if raw.block() {
            let Some(lang) = raw.lang().and_then(|l| Lang::from_tag(l.get())) else {
                return Ok(());
            };
            let (option_lines, code) = options::split_options(&text);
            let options = options::parse_options(&option_lines).map_err(|e| {
                anyhow::anyhow!("{}:{}: {}", self.project.display_path(ctx.path), line, e)
            })?;
            (lang, false, code, options)
        } else {
            let Some((lang, code)) = inline_cell(&text) else {
                return Ok(());
            };
            let options = CellOptions {
                eval: true,
                ..Default::default()
            };
            (lang, true, code.to_string(), options)
        };
        let n_option_lines = if inline { 0 } else { count_option_lines(&text) };
        let mut options = options;
        if ctx.preamble && options.setup.is_none() && options.session.is_none() {
            options.setup = Some(true);
        }
        let chapter = ctx.chapter.unwrap_or(ctx.root_chapter).to_string();
        if ctx.chapter.is_none() {
            self.add_chapter(&chapter);
        }
        self.doc.cells.push(Cell {
            lang,
            inline,
            code,
            options,
            file: ctx.path.to_path_buf(),
            line,
            code_line: first_text_line + n_option_lines,
            chapter,
        });
        Ok(())
    }
}

fn count_option_lines(text: &str) -> usize {
    options::split_options(text).0.len()
}

/// Recognize inline code: `{julia} expr`, `{r} expr`, `{python} expr`.
pub fn inline_cell(text: &str) -> Option<(Lang, &str)> {
    let rest = text.strip_prefix('{')?;
    let (tag, rest) = rest.split_once('}')?;
    let lang = Lang::from_tag(tag)?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some((lang, rest.trim()))
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("document")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            let p = dir.path().join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        let project = Project::from_root(&dir.path().join(files[0].0)).unwrap();
        (dir, project)
    }

    #[test]
    fn finds_block_and_inline_cells_in_document_order() {
        let (_d, p) = project_with(&[(
            "doc.typ",
            "= Title\n```julia\n#| echo: false\nx = 1\n```\nValue: `{julia} x + 1`.\n```python\nprint(1)\n```\n```rust\nfn main() {}\n```\n`plain` and `{rust} no`\n",
        )]);
        let doc = parse_document(&p).unwrap();
        let summary: Vec<_> = doc
            .cells
            .iter()
            .map(|c| (c.lang, c.inline, c.code.as_str(), c.line, c.code_line))
            .collect();
        assert_eq!(
            summary,
            vec![
                (Lang::Julia, false, "x = 1", 2, 4),
                (Lang::Julia, true, "x + 1", 6, 6),
                (Lang::Python, false, "print(1)", 7, 8),
            ]
        );
        assert_eq!(doc.cells[0].lookup_key(), "b:julia:x = 1");
        assert_eq!(doc.cells[1].lookup_key(), "i:julia:x + 1");
        assert_eq!(doc.chapters, vec!["doc"]);
    }

    #[test]
    fn comments_hide_cells() {
        let (_d, p) = project_with(&[(
            "doc.typ",
            "// ```julia\n/* ```julia\nx = 1\n``` */\n```r\ny <- 2\n```\n",
        )]);
        let doc = parse_document(&p).unwrap();
        assert_eq!(doc.cells.len(), 1);
        assert_eq!(doc.cells[0].lang, Lang::R);
    }

    #[test]
    fn includes_define_chapters_in_document_order() {
        let (_d, p) = project_with(&[
            (
                "book.typ",
                "```julia\na = 0\n```\n#include \"preamble.typ\"\n#include \"ch/one.typ\"\n```julia\nz = 9\n```\n#include \"ch/two.typ\"\n",
            ),
            ("preamble.typ", "```julia\nusing Statistics\n```\n"),
            ("ch/one.typ", "#include \"one-part.typ\"\n```julia\nb = 1\n```\n"),
            ("ch/one-part.typ", "```julia\nc = 2\n```\n"),
            ("ch/two.typ", "#figure[\n```r\nd <- 3\n```\n]\n"),
        ]);
        let doc = parse_document(&p).unwrap();
        let summary: Vec<_> = doc
            .cells
            .iter()
            .map(|c| (c.code.as_str(), c.chapter.as_str(), c.session()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("a = 0", "book", "book"),
                ("using Statistics", "preamble", "setup"),
                ("c = 2", "one", "one"),
                ("b = 1", "one", "one"),
                ("z = 9", "book", "book"),
                ("d <- 3", "two", "two"),
            ]
        );
        assert_eq!(doc.chapters, vec!["book", "preamble", "one", "two"]);
    }

    #[test]
    fn bad_options_report_file_and_line() {
        let (_d, p) = project_with(&[("doc.typ", "\n\n```julia\n#| ech: false\nx\n```\n")]);
        let err = parse_document(&p).unwrap_err().to_string();
        assert!(err.contains("doc.typ:3"), "{err}");
        assert!(err.contains("unknown option `ech`"), "{err}");
    }

    #[test]
    fn duplicate_labels_are_an_error() {
        let (_d, p) = project_with(&[(
            "doc.typ",
            "```julia\n#| label: fig-a\nx\n```\n```r\n#| label: \"fig-a\"\ny\n```\n",
        )]);
        let err = parse_document(&p).unwrap_err().to_string();
        assert!(err.contains("doc.typ:1") && err.contains("doc.typ:5"), "{err}");
    }

    #[test]
    fn missing_include_is_an_error() {
        let (_d, p) = project_with(&[("doc.typ", "#include \"nope.typ\"\n")]);
        assert!(parse_document(&p).is_err());
    }

    #[test]
    fn inline_cell_syntax() {
        assert_eq!(inline_cell("{julia} 1 + 1"), Some((Lang::Julia, "1 + 1")));
        assert_eq!(inline_cell("{r}  nrow(df) "), Some((Lang::R, "nrow(df)")));
        assert_eq!(inline_cell("{r}nrow(df)"), None);
        assert_eq!(inline_cell("{julia}"), None);
        assert_eq!(inline_cell("julia x"), None);
    }
}
