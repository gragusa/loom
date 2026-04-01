/// Content-addressed cache, scoped per chapter.
///
/// Layout on disk:
///
///   _jl_cache/
///     preamble/
///       manifest.json
///       figures/
///     intro/
///       manifest.json
///       figures/
///     ch2/
///       manifest.json
///       figures/
///
/// Each manifest.json is a HashMap<cell_id, CacheEntry>.
/// A cell is fresh when its code SHA-256 matches the stored hash
/// AND all referenced figure files still exist on disk.
use crate::daemon::{CellResult, Statement};
use crate::parser::{Book, Cell};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── Entry ─────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct CacheEntry {
    pub code_hash: String,
    pub stdout: String,
    pub stderr: String,
    pub figures: Vec<PathBuf>,
    pub error: Option<String>,
    #[serde(default)]
    pub statements: Vec<Statement>,
    #[serde(default)]
    pub typst_output: String,
}

// ── Per-chapter cache ─────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
pub struct ChapterCache {
    pub entries: HashMap<String, CacheEntry>,
}

impl ChapterCache {
    pub fn load(chapter_dir: &Path) -> Result<Self> {
        let manifest = chapter_dir.join("manifest.json");
        if !manifest.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(&manifest)
            .with_context(|| format!("Cannot read manifest: {}", manifest.display()))?;
        serde_json::from_str(&data).context("Corrupt chapter manifest")
    }

    pub fn save(&self, chapter_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(chapter_dir)?;
        let manifest = chapter_dir.join("manifest.json");
        std::fs::write(&manifest, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn is_fresh(&self, cell: &Cell) -> bool {
        match self.entries.get(&cell.id) {
            None => false,
            Some(e) => e.code_hash == cell_hash(cell) && e.figures.iter().all(|f| f.exists()),
        }
    }

    pub fn store(&mut self, cell: &Cell, result: &CellResult, chapter_dir: &Path) -> Result<()> {
        let fig_dir = chapter_dir.join("figures");
        std::fs::create_dir_all(&fig_dir)?;

        let mut stored_figs = Vec::new();
        for (n, fig) in result.figures.iter().enumerate() {
            let ext = fig.extension().and_then(|e| e.to_str()).unwrap_or("svg");
            let dest = fig_dir.join(format!("{}-{}.{}", cell.id, n, ext));
            if fig != &dest {
                std::fs::copy(fig, &dest).with_context(|| {
                    format!("Cannot copy figure {} → {}", fig.display(), dest.display())
                })?;
            }
            stored_figs.push(dest);
        }

        self.entries.insert(
            cell.id.clone(),
            CacheEntry {
                code_hash: cell_hash(cell),
                stdout: result.stdout.clone(),
                stderr: result.stderr.clone(),
                figures: stored_figs,
                error: result.error.clone(),
                statements: result.statements.clone(),
                typst_output: result.typst_output.clone(),
            },
        );
        self.save(chapter_dir)?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&CacheEntry> {
        self.entries.get(id)
    }
}

// ── Book-level cache façade ───────────────────────────────────────────────────

/// Loads and holds all per-chapter caches.
pub struct BookCache {
    root: PathBuf,
    chapters: HashMap<String, ChapterCache>,
}

impl BookCache {
    pub fn load(root: &Path, book: &Book) -> Result<Self> {
        let mut chapters = HashMap::new();
        for ch in &book.chapters {
            let ch_dir = root.join(ch);
            chapters.insert(ch.clone(), ChapterCache::load(&ch_dir)?);
        }
        Ok(Self {
            root: root.to_owned(),
            chapters,
        })
    }

    pub fn chapter(&self, name: &str) -> Option<&ChapterCache> {
        self.chapters.get(name)
    }

    pub fn chapter_mut(&mut self, name: &str) -> &mut ChapterCache {
        self.chapters.entry(name.to_string()).or_default()
    }

    pub fn chapter_dir(&self, chapter: &str) -> PathBuf {
        self.root.join(chapter)
    }

    pub fn is_fresh(&self, cell: &Cell) -> bool {
        self.chapters
            .get(&cell.chapter)
            .map(|c| c.is_fresh(cell))
            .unwrap_or(false)
    }

    pub fn store(&mut self, cell: &Cell, result: &CellResult) -> Result<()> {
        let dir = self.chapter_dir(&cell.chapter);
        self.chapter_mut(&cell.chapter).store(cell, result, &dir)
    }

    /// Find the index of the first stale cell within a chapter.
    pub fn first_stale_in_chapter<'a>(&self, cells: &'a [Cell]) -> Option<usize> {
        cells.iter().position(|c| !self.is_fresh(c))
    }
}

// ── Hashing ───────────────────────────────────────────────────────────────────

/// Hash a cell's code AND its options (so changing fig-width invalidates cache).
pub fn cell_hash(cell: &Cell) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cell.code.as_bytes());
    // Sort options for deterministic hashing.
    let mut opts: Vec<_> = cell.options.iter().collect();
    opts.sort_by_key(|(k, _)| (*k).clone());
    for (k, v) in opts {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b";");
    }
    hex::encode(hasher.finalize())
}
