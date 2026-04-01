/// Chapter runner — executes cells and updates the cache.
///
/// Each chapter has its own session per language.  Julia and R cells
/// can coexist in the same chapter file; they are routed to the
/// appropriate daemon client.
///
/// Preamble code is replayed per-language into fresh sessions.
use crate::cache::BookCache;
use crate::codegen;
use crate::config::StyleConfig;
use crate::daemon::DaemonClient;
use crate::parser::{Book, Cell, Language};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;

/// Holds the daemon clients for each supported language.
pub struct Clients {
    pub julia: Option<DaemonClient>,
    pub r: Option<DaemonClient>,
    pub fig_width: f64,
    pub fig_height: f64,
    pub style: StyleConfig,
}

impl Clients {
    fn client_for(&self, lang: Language) -> Result<&DaemonClient> {
        match lang {
            Language::Julia => self.julia.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "Julia cell found but Julia is not configured or not required for this run."
                )
            }),
            Language::R => self.r.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "R cell found but R is not configured. \
                     Set `r = \"Rscript\"` in loom.toml."
                )
            }),
        }
    }
}

// ── Public entry points ───────────────────────────────────────────────────────

/// Run a single chapter.
pub async fn run_chapter(
    chapter: &str,
    book: &Book,
    cache: &mut BookCache,
    _cache_root: &Path,
    clients: &Clients,
    force: bool,
) -> Result<()> {
    let cells = book.chapter_cells(chapter);
    if cells.is_empty() {
        log::debug!("Chapter '{}' has no cells.", chapter);
        return Ok(());
    }

    let first_stale = if force {
        Some(0)
    } else {
        cache.first_stale_in_chapter(cells)
    };

    let Some(start) = first_stale else {
        log::info!("Chapter '{}': all {} cells fresh.", chapter, cells.len());
        return Ok(());
    };

    log::info!(
        "Chapter '{}': re-running {}/{} cells (first stale: '{}').",
        chapter,
        cells.len() - start,
        cells.len(),
        cells[start].id,
    );

    // Reset each distinct session actually used by the chapter, replaying the
    // language-specific preamble into non-preamble sessions.
    for (language, session) in session_targets(cells) {
        let client = clients.client_for(language)?;
        let preamble_code = session_seed(book, language, &session, chapter);
        client
            .reset_session(&session, &preamble_code)
            .await
            .with_context(|| {
                format!(
                    "failed to reset {:?} session '{}' before chapter '{}'",
                    language, session, chapter
                )
            })?;
    }

    for cell in cells.iter() {
        let client = clients.client_for(cell.language)?;
        log::debug!("  Running {:?} cell '{}'", cell.language, cell.id);
        let result = client
            .run_cell(cell, clients.fig_width, clients.fig_height)
            .await
            .with_context(|| cell_context(chapter, cell, "daemon transport failure"))?;

        if let Some(err) = result.error.clone() {
            anyhow::bail!(
                "{}: {}",
                cell_context(chapter, cell, "cell execution failed"),
                err
            );
        }

        cache
            .store(cell, &result)
            .with_context(|| cell_context(chapter, cell, "failed to update cache"))?;
    }

    log::info!("Chapter '{}': done.", chapter);
    Ok(())
}

/// Run the preamble first, then all other chapters.
pub async fn run_all(
    book: &Book,
    cache: &mut BookCache,
    cache_root: &Path,
    clients: &Clients,
    force: bool,
    data_file: &Path,
) -> Result<()> {
    if book.chapters.contains(&"preamble".to_string()) {
        run_chapter("preamble", book, cache, cache_root, clients, force).await?;
    }

    for chapter in &book.chapters {
        if chapter == "preamble" {
            continue;
        }
        run_chapter(chapter, book, cache, cache_root, clients, force).await?;
    }

    codegen::write_cache_typ(cache_root, book, cache)?;
    codegen::write_style_typ(&clients.style, data_file.parent().unwrap_or(Path::new(".")))?;
    codegen::write_data_typ(data_file, cache_root)?;
    Ok(())
}

/// Run only chapters whose source files include `changed_path`.
pub async fn run_affected(
    changed_path: &Path,
    book: &Book,
    cache: &mut BookCache,
    cache_root: &Path,
    clients: &Clients,
    data_file: &Path,
) -> Result<bool> {
    let directly_changed: HashSet<String> = book
        .all_cells()
        .filter(|c| c.source_file == changed_path)
        .map(|c| c.chapter.clone())
        .collect();

    if directly_changed.is_empty() {
        return Ok(false);
    }

    if directly_changed.contains("preamble") {
        log::info!("Preamble changed — re-running all chapters.");
        run_all(book, cache, cache_root, clients, false, data_file).await?;
        return Ok(true);
    }

    let mut affected = Vec::new();
    let mut active_sessions: HashSet<(Language, String)> = HashSet::new();

    for chapter in &book.chapters {
        let cells = book.chapter_cells(chapter);
        let chapter_sessions = session_keys(cells);
        let is_direct = directly_changed.contains(chapter);
        let is_downstream_shared = !active_sessions.is_empty()
            && chapter_sessions
                .iter()
                .any(|key| active_sessions.contains(key));

        if is_direct || is_downstream_shared {
            affected.push(chapter.clone());
            active_sessions.extend(chapter_sessions);
        }
    }

    let mut any = false;
    for chapter in &affected {
        run_chapter(chapter, book, cache, cache_root, clients, false).await?;
        any = true;
    }

    if any {
        codegen::write_cache_typ(cache_root, book, cache)?;
        codegen::write_style_typ(&clients.style, data_file.parent().unwrap_or(Path::new(".")))?;
        codegen::write_data_typ(data_file, cache_root)?;
    }

    Ok(any)
}

// ── Internal ──────────────────────────────────────────────────────────────────

/// Build the code needed to reconstruct a session before re-running `chapter`.
fn session_seed(book: &Book, lang: Language, session: &str, chapter: &str) -> String {
    if chapter == "preamble" {
        return String::new();
    }

    let mut chunks = Vec::new();

    for current in &book.chapters {
        if current == chapter {
            break;
        }

        if current == "preamble" {
            let code = book
                .chapter_cells("preamble")
                .iter()
                .filter(|c| c.language == lang)
                .map(|c| c.code.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if !code.is_empty() {
                chunks.push(code);
            }
            continue;
        }

        let code = book
            .chapter_cells(current)
            .iter()
            .filter(|c| c.language == lang && c.session == session)
            .map(|c| c.code.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !code.is_empty() {
            chunks.push(code);
        }
    }

    chunks.join("\n")
}

fn session_targets(cells: &[Cell]) -> Vec<(Language, String)> {
    let mut seen: HashSet<(Language, String)> = HashSet::new();
    let mut targets = Vec::new();

    for cell in cells {
        let target = (cell.language, cell.session.clone());
        if seen.insert(target.clone()) {
            targets.push(target);
        }
    }

    targets
}

fn cell_context(chapter: &str, cell: &Cell, summary: &str) -> String {
    format!(
        "{} in chapter '{}' for cell '{}' at {}:{}",
        summary,
        chapter,
        cell.id,
        cell.source_file.display(),
        cell.line
    )
}

fn session_keys(cells: &[Cell]) -> HashSet<(Language, String)> {
    cells
        .iter()
        .map(|cell| (cell.language, cell.session.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::CellKind;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn test_cell(id: &str, language: Language, session: &str) -> Cell {
        Cell {
            id: id.to_string(),
            kind: CellKind::Run,
            language,
            code: String::new(),
            session: session.to_string(),
            chapter: "chapter".to_string(),
            source_file: PathBuf::from("chapter.typ"),
            line: 1,
            options: HashMap::new(),
        }
    }

    #[test]
    fn session_targets_are_unique_and_ordered() {
        let cells = vec![
            test_cell("a", Language::Julia, "chapter"),
            test_cell("b", Language::Julia, "chapter"),
            test_cell("c", Language::R, "chapter"),
            test_cell("d", Language::Julia, "shared"),
            test_cell("e", Language::R, "chapter"),
        ];

        assert_eq!(
            session_targets(&cells),
            vec![
                (Language::Julia, "chapter".to_string()),
                (Language::R, "chapter".to_string()),
                (Language::Julia, "shared".to_string()),
            ]
        );
    }

    #[test]
    fn session_seed_for_preamble_is_empty() {
        let book = Book::default();
        assert_eq!(
            session_seed(&book, Language::Julia, "shared", "preamble"),
            ""
        );
    }

    #[test]
    fn session_seed_replays_preamble_and_prior_shared_cells() {
        let mut book = Book::default();
        book.chapters = vec![
            "preamble".to_string(),
            "intro".to_string(),
            "analysis".to_string(),
        ];
        book.cells.insert(
            "preamble".to_string(),
            vec![Cell {
                code: "using Foo".to_string(),
                session: "preamble".to_string(),
                chapter: "preamble".to_string(),
                ..test_cell("p", Language::Julia, "preamble")
            }],
        );
        book.cells.insert(
            "intro".to_string(),
            vec![
                Cell {
                    code: "x = 1".to_string(),
                    chapter: "intro".to_string(),
                    ..test_cell("a", Language::Julia, "shared")
                },
                Cell {
                    code: "y = 2".to_string(),
                    chapter: "intro".to_string(),
                    ..test_cell("b", Language::R, "shared")
                },
            ],
        );
        book.cells.insert(
            "analysis".to_string(),
            vec![Cell {
                code: "z = x + 1".to_string(),
                chapter: "analysis".to_string(),
                ..test_cell("c", Language::Julia, "shared")
            }],
        );

        assert_eq!(
            session_seed(&book, Language::Julia, "shared", "analysis"),
            "using Foo\nx = 1"
        );
    }
}
