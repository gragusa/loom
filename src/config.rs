//! Project layout and `loom.toml` configuration.
//!
//! A project is the directory that contains the root `.typ` file. `loom.toml`
//! is read from that directory, kernels run with it as working directory, and
//! all generated files live under its `.loom/` subdirectory.

use crate::syntax::Lang;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = "loom.toml";

/// Paths of one project.
#[derive(Debug, Clone)]
pub struct Project {
    /// Root `.typ` file (absolute).
    pub root_file: PathBuf,
    /// Directory containing the root file (absolute).
    pub dir: PathBuf,
}

impl Project {
    pub fn from_root(root: &Path) -> Result<Self> {
        let root_file = root
            .canonicalize()
            .with_context(|| format!("cannot find root file {}", root.display()))?;
        if root_file.extension().and_then(|e| e.to_str()) != Some("typ") {
            bail!("root file must be a .typ file: {}", root_file.display());
        }
        let dir = root_file
            .parent()
            .context("root file has no parent directory")?
            .to_path_buf();
        Ok(Self { root_file, dir })
    }

    /// The `.loom/` directory: support library, data file, figures.
    pub fn loom_dir(&self) -> PathBuf {
        self.dir.join(".loom")
    }
    /// Data file read by the Typst library.
    pub fn cells_file(&self) -> PathBuf {
        self.loom_dir().join("cells.json")
    }
    pub fn figures_dir(&self) -> PathBuf {
        self.loom_dir().join("figures")
    }
    /// Stored cell results keyed by cache key.
    pub fn store_file(&self) -> PathBuf {
        self.loom_dir().join("cache").join("entries.json")
    }
    /// Kernel scripts, logs, registry files, and image scratch space.
    pub fn kernels_dir(&self) -> PathBuf {
        self.loom_dir().join("kernels")
    }

    /// Path relative to the project directory, for messages.
    pub fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.dir)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

/// Image formats a kernel can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FigFormat {
    Svg,
    Png,
    Pdf,
}

impl FigFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            FigFormat::Svg => "svg",
            FigFormat::Png => "png",
            FigFormat::Pdf => "pdf",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "svg" => Some(FigFormat::Svg),
            "png" => Some(FigFormat::Png),
            "pdf" => Some(FigFormat::Pdf),
            _ => None,
        }
    }
}

/// Project-wide defaults for execution options; cell options override them.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ExecuteDefaults {
    pub fig_width: f64,
    pub fig_height: f64,
    pub fig_format: FigFormat,
    pub fig_dpi: f64,
    pub error: bool,
}

impl Default for ExecuteDefaults {
    fn default() -> Self {
        Self {
            fig_width: 7.0,
            fig_height: 5.0,
            fig_format: FigFormat::Svg,
            fig_dpi: 300.0,
            error: false,
        }
    }
}

/// How to start the kernel of one language:
/// `command args... <script> <kernel args>`. Unset fields take the
/// language's defaults (`LangConfig::command_for`, `LangConfig::args_for`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LangConfig {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
}

impl LangConfig {
    pub fn command_for(&self, lang: Lang) -> String {
        self.command.clone().unwrap_or_else(|| {
            match lang {
                Lang::Julia => "julia",
                Lang::R => "Rscript",
                Lang::Python => "python3",
            }
            .to_string()
        })
    }

    pub fn args_for(&self, lang: Lang) -> Vec<String> {
        self.args.clone().unwrap_or_else(|| match lang {
            // One interactive thread lets the kernel's parent watchdog run
            // while a cell computes.
            Lang::Julia => vec![
                "--startup-file=no".into(),
                "--threads=1,1".into(),
                "--project=@.".into(),
            ],
            Lang::R | Lang::Python => vec![],
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TypstConfig {
    pub command: Option<String>,
}

impl TypstConfig {
    pub fn command(&self) -> String {
        self.command.clone().unwrap_or_else(|| "typst".into())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub execute: ExecuteDefaults,
    pub julia: LangConfig,
    pub r: LangConfig,
    pub python: LangConfig,
    pub typst: TypstConfig,
}

impl Config {
    /// Read `loom.toml` from the project directory; defaults when absent.
    pub fn load(project: &Project) -> Result<Self> {
        let path = project.dir.join(CONFIG_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        cfg.validate()
            .with_context(|| format!("invalid {}", path.display()))?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let e = &self.execute;
        if !(e.fig_width > 0.0 && e.fig_height > 0.0 && e.fig_dpi > 0.0) {
            bail!("[execute] fig-width, fig-height and fig-dpi must be positive");
        }
        Ok(())
    }

    pub fn lang(&self, lang: Lang) -> &LangConfig {
        match lang {
            Lang::Julia => &self.julia,
            Lang::R => &self.r,
            Lang::Python => &self.python,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_config_keeps_defaults() {
        let cfg: Config = toml::from_str("[execute]\nfig-width = 5\n").unwrap();
        assert_eq!(cfg.execute.fig_width, 5.0);
        assert_eq!(cfg.execute.fig_height, 5.0);
        assert_eq!(cfg.julia.command_for(Lang::Julia), "julia");
        assert!(cfg.julia.args_for(Lang::Julia).contains(&"--project=@.".to_string()));
    }

    #[test]
    fn template_parses() {
        let cfg: Config = toml::from_str(include_str!("../templates/loom.toml")).unwrap();
        assert_eq!(cfg.r.command_for(Lang::R), "Rscript");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("[execute]\nfig_width = 5\n").is_err());
        assert!(toml::from_str::<Config>("julia_port = 2159\n").is_err());
    }
}
