//! Package environment of stored results.
//!
//! After every cell, the kernel lists the packages loaded in its process
//! (see "State and packages" in `kernels/PROTOCOL.md`). loom fingerprints
//! each one and stores the set with the result, together with the output of
//! `<command> <args> --version`. Before reusing a result, loom recomputes the
//! fingerprints; a result whose packages or language version changed is
//! stale, and the kernel that produced it must be restarted, because a
//! process cannot replace a package it has loaded.
//!
//! Fingerprints:
//! - `manifest` (Julia): the package's manifest entry, plus the source tree
//!   when the entry tracks a path (`Pkg.develop`);
//! - `file`: the contents of the file;
//! - `tree`: the relative path, size, and modification time of every file
//!   under the directory, skipping hidden entries and `__pycache__`.

use crate::config::{Config, Project};
use crate::syntax::Lang;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: Option<String>,
    pub kind: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    pub fingerprint: String,
}

/// The environment a result was computed in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvRecord {
    pub language_version: String,
    pub packages: Vec<Package>,
}

impl EnvRecord {
    /// Content address under which the store keeps the record.
    pub fn id(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("EnvRecord serializes");
        hex::encode(&Sha256::digest(&bytes)[..12])
    }
}

/// Why a stored environment no longer matches.
#[derive(Debug, Clone)]
pub struct Mismatch(pub String);

/// Computes and caches fingerprints and language versions for one run.
pub struct EnvChecker<'a> {
    project: &'a Project,
    cfg: &'a Config,
    versions: HashMap<Lang, String>,
    fingerprints: HashMap<(String, String, Option<String>), Result<String, String>>,
    manifests: HashMap<PathBuf, toml::Table>,
    verdicts: HashMap<String, Option<Mismatch>>,
}

impl<'a> EnvChecker<'a> {
    pub fn new(project: &'a Project, cfg: &'a Config) -> Self {
        Self {
            project,
            cfg,
            versions: HashMap::new(),
            fingerprints: HashMap::new(),
            manifests: HashMap::new(),
            verdicts: HashMap::new(),
        }
    }

    pub fn language_version(&mut self, lang: Lang) -> Result<String> {
        if let Some(v) = self.versions.get(&lang) {
            return Ok(v.clone());
        }
        let lc = self.cfg.lang(lang);
        let command = lc.command_for(lang, &self.project.dir);
        let out = std::process::Command::new(&command)
            .args(lc.args_for(lang))
            .arg("--version")
            .current_dir(&self.project.dir)
            .output()
            .with_context(|| format!("cannot run `{command} --version`"))?;
        if !out.status.success() {
            bail!(
                "`{command} --version` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let mut v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if v.is_empty() {
            v = err;
        }
        self.versions.insert(lang, v.clone());
        Ok(v)
    }

    /// Build the record for a kernel reply's `packages` list.
    pub fn record(&mut self, lang: Lang, packages: &[Value]) -> Result<EnvRecord> {
        let mut out = Vec::with_capacity(packages.len());
        for p in packages {
            let field = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
            let name = field("name").context("package entry without name")?;
            let kind = field("kind").context("package entry without kind")?;
            let path = field("path").context("package entry without path")?;
            let uuid = field("uuid");
            let fingerprint = self
                .fingerprint(&kind, &path, uuid.as_deref())
                .map_err(|e| anyhow::anyhow!("package {name}: {e}"))?;
            out.push(Package {
                name,
                version: field("version"),
                kind,
                path,
                uuid,
                fingerprint,
            });
        }
        Ok(EnvRecord {
            language_version: self.language_version(lang)?,
            packages: out,
        })
    }

    /// `None` when the environment still matches the record.
    pub fn check(&mut self, lang: Lang, id: &str, record: &EnvRecord) -> Result<Option<Mismatch>> {
        if let Some(v) = self.verdicts.get(id) {
            return Ok(v.clone());
        }
        let current = self.language_version(lang)?;
        let verdict = if current != record.language_version {
            Some(Mismatch(format!(
                "{} changed from `{}` to `{}`",
                lang.name(),
                record.language_version,
                current
            )))
        } else {
            let mut found = None;
            for p in &record.packages {
                let now = self.fingerprint(&p.kind, &p.path, p.uuid.as_deref());
                if now.as_deref().ok() != Some(p.fingerprint.as_str()) {
                    found = Some(Mismatch(format!(
                        "package {} changed ({})",
                        p.name,
                        match now {
                            Ok(_) => "different contents".to_string(),
                            Err(e) => e,
                        }
                    )));
                    break;
                }
            }
            found
        };
        self.verdicts.insert(id.to_string(), verdict.clone());
        Ok(verdict)
    }

    fn fingerprint(&mut self, kind: &str, path: &str, uuid: Option<&str>) -> Result<String, String> {
        let key = (kind.to_string(), path.to_string(), uuid.map(str::to_string));
        if let Some(r) = self.fingerprints.get(&key) {
            return r.clone();
        }
        let r = match kind {
            "file" => file_fingerprint(Path::new(path)),
            "tree" => tree_fingerprint(Path::new(path)),
            "manifest" => match uuid {
                Some(u) => self.manifest_fingerprint(Path::new(path), u),
                None => Err("manifest entry without uuid".into()),
            },
            other => Err(format!("unknown package kind `{other}`")),
        };
        self.fingerprints.insert(key, r.clone());
        r
    }

    fn manifest_fingerprint(&mut self, path: &Path, uuid: &str) -> Result<String, String> {
        if !self.manifests.contains_key(path) {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let table: toml::Table = text
                .parse()
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            self.manifests.insert(path.to_path_buf(), table);
        }
        let table = &self.manifests[path];
        // Manifest format 2 keeps packages under `deps`; format 1 at top level.
        let deps = table.get("deps").and_then(toml::Value::as_table).unwrap_or(table);
        let entry = deps
            .values()
            .filter_map(toml::Value::as_array)
            .flatten()
            .filter_map(toml::Value::as_table)
            .find(|e| e.get("uuid").and_then(toml::Value::as_str) == Some(uuid))
            .ok_or_else(|| format!("no longer in {}", path.display()))?;
        let canonical: BTreeMap<&String, String> =
            entry.iter().map(|(k, v)| (k, v.to_string())).collect();
        let mut h = Sha256::new();
        h.update(serde_json::to_vec(&canonical).expect("map serializes"));
        if let Some(dev) = entry.get("path").and_then(toml::Value::as_str) {
            let dir = path.parent().unwrap_or(Path::new(".")).join(dev);
            h.update(tree_fingerprint(&dir)?.as_bytes());
        }
        Ok(hex::encode(&h.finalize()[..12]))
    }
}

fn file_fingerprint(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(hex::encode(&Sha256::digest(&bytes)[..12]))
}

fn tree_fingerprint(path: &Path) -> Result<String, String> {
    if path.is_file() {
        return file_fingerprint(path);
    }
    let mut lines = Vec::new();
    walk(path, path, &mut lines).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    lines.sort();
    let mut h = Sha256::new();
    for l in &lines {
        h.update(l.as_bytes());
        h.update(b"\n");
    }
    Ok(hex::encode(&h.finalize()[..12]))
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "__pycache__" {
            continue;
        }
        let path = entry.path();
        let meta = entry.metadata()?;
        if meta.is_dir() {
            walk(root, &path, out)?;
        } else if meta.is_file() {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string();
            out.push(format!("{rel}\t{}\t{mtime}", meta.len()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_fingerprint_tracks_contents_and_ignores_hidden_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/a.jl"), "x").unwrap();
        let f1 = tree_fingerprint(d.path()).unwrap();
        std::fs::write(d.path().join(".hidden"), "y").unwrap();
        assert_eq!(f1, tree_fingerprint(d.path()).unwrap());
        std::fs::write(d.path().join("src/b.jl"), "z").unwrap();
        assert_ne!(f1, tree_fingerprint(d.path()).unwrap());
    }

    #[test]
    fn manifest_fingerprint_follows_the_entry() {
        let d = tempfile::tempdir().unwrap();
        let m = d.path().join("Manifest.toml");
        let write = |sha: &str| {
            std::fs::write(
                &m,
                format!(
                    "manifest_format = \"2.0\"\n[[deps.A]]\nuuid = \"u-a\"\nversion = \"1.0.0\"\ngit-tree-sha1 = \"{sha}\"\n[[deps.B]]\nuuid = \"u-b\"\nversion = \"2.0.0\"\n"
                ),
            )
            .unwrap()
        };
        let project = Project {
            root_file: d.path().join("x.typ"),
            dir: d.path().to_path_buf(),
        };
        let cfg = Config::default();
        write("aaa");
        let a1 = EnvChecker::new(&project, &cfg).manifest_fingerprint(&m, "u-a").unwrap();
        let b1 = EnvChecker::new(&project, &cfg).manifest_fingerprint(&m, "u-b").unwrap();
        write("bbb");
        let a2 = EnvChecker::new(&project, &cfg).manifest_fingerprint(&m, "u-a").unwrap();
        let b2 = EnvChecker::new(&project, &cfg).manifest_fingerprint(&m, "u-b").unwrap();
        assert_ne!(a1, a2);
        assert_eq!(b1, b2);
        assert!(EnvChecker::new(&project, &cfg).manifest_fingerprint(&m, "u-c").is_err());
    }
}
