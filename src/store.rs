//! Stored cell results and the data file read by Typst.
//!
//! `.loom/cache/entries.json` maps cache keys (see `plan`) to results. Because
//! keys are content-addressed, an entry never needs to be invalidated: an
//! edit produces a new key. Figures are stored once under the hash of their
//! bytes in `.loom/figures/`, so a changed figure always has a new path.
//!
//! `.loom/cells.json` is derived from the document and the store after every
//! run. It maps each cell's lookup key to the list of its results, one per
//! occurrence of that key in document order (`null` when the cell has no
//! stored result).

use crate::config::Project;
use crate::plan::Plan;
use crate::syntax::Document;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

const STORE_VERSION: u32 = 1;
const DATA_VERSION: u32 = 1;

/// Results that are not referenced by the current document are kept, up to
/// this many, so that undoing an edit finds the earlier result.
pub const KEEP_UNREFERENCED: usize = 200;

const IMAGE_TYPES: &[(&str, &str)] = &[
    ("image/svg+xml", "svg"),
    ("image/png", "png"),
    ("application/pdf", "pdf"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub lang: String,
    pub session: String,
    /// User code raised an error.
    pub failed: bool,
    /// The cell declared `error: true`, so a failure is expected output.
    pub error_ok: bool,
    /// Kernel outputs, with image paths relative to `.loom/`.
    pub outputs: Vec<Value>,
    /// Execution options as written in the cell.
    pub exec: BTreeMap<String, String>,
    pub fig_width: f64,
    pub fig_height: f64,
    /// Unix time of the last run that referenced this entry.
    pub used: u64,
}

impl Entry {
    /// Whether the result can be reused, i.e. it is not an unexpected failure.
    pub fn is_good(&self) -> bool {
        !self.failed || self.error_ok
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Store {
    version: u32,
    entries: BTreeMap<String, Entry>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

impl Store {
    pub fn load(project: &Project) -> Result<Self> {
        let path = project.store_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let store: Store = serde_json::from_str(&text).with_context(|| {
            format!(
                "corrupt result store {}; delete it to start over",
                path.display()
            )
        })?;
        if store.version != STORE_VERSION {
            bail!(
                "result store {} has version {}, expected {}; delete it to start over",
                path.display(),
                store.version,
                STORE_VERSION
            );
        }
        Ok(store)
    }

    pub fn save(&self, project: &Project) -> Result<()> {
        write_atomic(&project.store_file(), &serde_json::to_vec(self)?)
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.get(key)
    }

    pub fn insert(&mut self, key: String, entry: Entry) {
        self.entries.insert(key, entry);
    }

    /// Mark the entries of the current document as used now.
    pub fn touch(&mut self, plan: &Plan) {
        let now = now();
        for key in plan.cell_keys.iter().flatten() {
            if let Some(e) = self.entries.get_mut(key) {
                e.used = now;
            }
        }
    }

    /// Drop unreferenced entries beyond the `keep` most recently used, then
    /// delete figure files that no remaining entry references. Returns the
    /// number of entries and figures removed.
    pub fn prune(&mut self, project: &Project, plan: &Plan, keep: usize) -> Result<(usize, usize)> {
        let live: HashSet<&String> = plan.cell_keys.iter().flatten().collect();
        let mut unreferenced: Vec<(u64, String)> = self
            .entries
            .iter()
            .filter(|(k, _)| !live.contains(k))
            .map(|(k, e)| (e.used, k.clone()))
            .collect();
        unreferenced.sort_by(|a, b| b.0.cmp(&a.0));
        let mut removed = 0;
        for (_, key) in unreferenced.into_iter().skip(keep) {
            self.entries.remove(&key);
            removed += 1;
        }

        let referenced: HashSet<String> = self
            .entries
            .values()
            .flat_map(|e| e.outputs.iter())
            .flat_map(image_files)
            .collect();
        let mut removed_figs = 0;
        let fig_dir = project.figures_dir();
        if fig_dir.exists() {
            for f in std::fs::read_dir(&fig_dir)? {
                let f = f?;
                let rel = format!("figures/{}", f.file_name().to_string_lossy());
                if !referenced.contains(&rel) {
                    std::fs::remove_file(f.path())?;
                    removed_figs += 1;
                }
            }
        }
        Ok((removed, removed_figs))
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn image_files(output: &Value) -> Vec<String> {
    let Some(data) = output.get("data").and_then(Value::as_object) else {
        return Vec::new();
    };
    IMAGE_TYPES
        .iter()
        .filter_map(|(mime, _)| data.get(*mime))
        .filter_map(|v| v.get("file").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// Move the image files a kernel wrote into `.loom/figures/`, named by the
/// hash of their contents, and rewrite the outputs to refer to them by a
/// path relative to `.loom/` (where the Typst library resolves them).
pub fn import_figures(project: &Project, outputs: &mut [Value]) -> Result<()> {
    let fig_dir = project.figures_dir();
    for output in outputs.iter_mut() {
        let Some(data) = output.get_mut("data").and_then(Value::as_object_mut) else {
            continue;
        };
        for (mime, ext) in IMAGE_TYPES {
            let Some(spec) = data.get_mut(*mime) else {
                continue;
            };
            let src = spec
                .get("file")
                .and_then(Value::as_str)
                .with_context(|| format!("kernel returned {mime} without a file"))?
                .to_string();
            let bytes = std::fs::read(&src)
                .with_context(|| format!("kernel image {src} cannot be read"))?;
            if bytes.is_empty() {
                bail!("kernel image {src} is empty");
            }
            let name = format!("{}.{ext}", hex::encode(&Sha256::digest(&bytes)[..12]));
            std::fs::create_dir_all(&fig_dir)?;
            let dest = fig_dir.join(&name);
            if !dest.exists() {
                write_atomic(&dest, &bytes)?;
            }
            std::fs::remove_file(&src).with_context(|| format!("cannot remove {src}"))?;
            *spec = json!({ "file": format!("figures/{name}") });
        }
    }
    Ok(())
}

/// Write `.loom/cells.json` for the current document.
pub fn write_cells_json(project: &Project, doc: &Document, plan: &Plan, store: &Store) -> Result<()> {
    let mut cells: Map<String, Value> = Map::new();
    for (i, cell) in doc.cells.iter().enumerate() {
        let Some(key) = &plan.cell_keys[i] else {
            continue; // eval: false cells are rendered without a lookup
        };
        let view = match store.get(key) {
            None => Value::Null,
            Some(e) => json!({
                "status": if e.is_good() { "ok" } else { "error" },
                "session": e.session,
                "exec": e.exec,
                "fig-width": e.fig_width,
                "fig-height": e.fig_height,
                "outputs": e.outputs,
            }),
        };
        match cells
            .entry(cell.lookup_key())
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            Value::Array(list) => list.push(view),
            _ => unreachable!("cells.json values are arrays"),
        }
    }
    let data = json!({ "version": DATA_VERSION, "cells": cells });
    write_atomic(&project.cells_file(), &serde_json::to_vec(&data)?)
}

/// Write the file next to its destination and rename it into place, so that
/// a reader (Tinymist, recompiling on every change) never sees a partial file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}
