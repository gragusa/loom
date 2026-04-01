/// Per-project configuration loaded from `loom.toml`.
///
/// Resolution order: chunk option > TOML value > hardcoded default.
use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = "loom.toml";

// Defaults
const DEFAULT_JULIA: &str = "julia";
const DEFAULT_JULIA_PORT: u16 = 2159;
const DEFAULT_R: &str = "Rscript";
const DEFAULT_R_PORT: u16 = 2160;
const DEFAULT_CACHE_DIR: &str = ".loom/_loom_cache";
const DEFAULT_DATA_FILE: &str = ".loom/_loom_data.typ";
const DEFAULT_FIG_WIDTH: f64 = 7.0;
const DEFAULT_FIG_HEIGHT: f64 = 5.0;
const DEFAULT_IDLE_TIMEOUT: u64 = 1800; // 30 minutes
const DEFAULT_PRESTART_ALL_LANGUAGES: bool = false;

/// Style settings that map to Typst state variables.
/// Values are raw Typst expressions (strings).
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(default)]
pub struct StyleConfig {
    /// Julia console code font size (e.g. "9pt", "14pt")
    pub jl_code_size: Option<String>,
    /// Julia console prompt font size
    pub jl_prompt_size: Option<String>,
    /// Julia console prompt text
    pub jl_prompt_text: Option<String>,
    /// R console code font size
    pub r_code_size: Option<String>,
    /// R console prompt font size
    pub r_prompt_size: Option<String>,
    /// R console prompt text
    pub r_prompt_text: Option<String>,
    /// Console output color (e.g. "luma(100)", "rgb(\"#666\")")
    pub output_color: Option<String>,
    /// Console block fill (e.g. "luma(248)")
    pub block_fill: Option<String>,
    /// Console block inset (e.g. "8pt")
    pub block_inset: Option<String>,
    /// Console block radius (e.g. "2pt")
    pub block_radius: Option<String>,
    /// Console block stroke (e.g. "0.5pt + luma(220)")
    pub block_stroke: Option<String>,
    /// Line spacing inside console blocks (e.g. "0.55em")
    pub line_spacing: Option<String>,
    /// Caption font size (e.g. "8pt")
    pub caption_size: Option<String>,
    /// Default caption vertical offset for margin mode (e.g. "1.75em")
    pub caption_dy: Option<String>,
    /// Gap between figure and inline caption (e.g. "4pt", "0pt", "-2pt")
    pub caption_gap: Option<String>,
    /// Monospace font for console blocks (e.g. "JuliaMono", "Fira Code")
    pub font: Option<String>,
}

/// Raw TOML representation (all fields optional).
#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct TomlConfig {
    julia: Option<String>,
    julia_port: Option<u16>,
    r: Option<String>,
    r_port: Option<u16>,
    cache_dir: Option<String>,
    data_file: Option<String>,
    fig_width: Option<f64>,
    fig_height: Option<f64>,
    /// Idle timeout in seconds (0 = no timeout). Default: 1800 (30 min).
    idle_timeout: Option<u64>,
    /// Start configured daemons even if the current document does not yet use them.
    prestart_all_languages: Option<bool>,
    style: Option<StyleConfig>,
}

/// Resolved configuration with concrete values.
#[derive(Debug, Clone)]
pub struct Config {
    pub julia: String,
    pub julia_port: u16,
    pub r: Option<String>,
    pub r_port: u16,
    pub cache_dir: PathBuf,
    pub data_file: PathBuf,
    /// Default figure width in inches (used when chunk doesn't specify fig-width).
    pub fig_width: f64,
    /// Default figure height in inches.
    pub fig_height: f64,
    /// Daemon idle timeout in seconds (0 = no timeout).
    pub idle_timeout: u64,
    /// Start configured daemons even if the current document does not yet use them.
    pub prestart_all_languages: bool,
    /// Style settings for Typst rendering.
    pub style: StyleConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            julia: DEFAULT_JULIA.to_string(),
            julia_port: DEFAULT_JULIA_PORT,
            r: None,
            r_port: DEFAULT_R_PORT,
            cache_dir: PathBuf::from(DEFAULT_CACHE_DIR),
            data_file: PathBuf::from(DEFAULT_DATA_FILE),
            fig_width: DEFAULT_FIG_WIDTH,
            fig_height: DEFAULT_FIG_HEIGHT,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            prestart_all_languages: DEFAULT_PRESTART_ALL_LANGUAGES,
            style: StyleConfig::default(),
        }
    }
}

impl Config {
    /// Load config from `loom.toml` (if it exists), then override with CLI values.
    pub fn load(
        cli_port: Option<u16>,
        cli_cache_dir: Option<&Path>,
        cli_idle_timeout: Option<u64>,
    ) -> Result<Self> {
        let toml = load_toml()?;
        Ok(Self {
            julia: toml.julia.unwrap_or_else(|| DEFAULT_JULIA.to_string()),
            julia_port: cli_port.or(toml.julia_port).unwrap_or(DEFAULT_JULIA_PORT),
            r: toml.r.or(Some(DEFAULT_R.to_string())),
            r_port: toml.r_port.unwrap_or(DEFAULT_R_PORT),
            cache_dir: cli_cache_dir
                .map(PathBuf::from)
                .or_else(|| toml.cache_dir.map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from(DEFAULT_CACHE_DIR)),
            data_file: toml
                .data_file
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_FILE)),
            fig_width: toml.fig_width.unwrap_or(DEFAULT_FIG_WIDTH),
            fig_height: toml.fig_height.unwrap_or(DEFAULT_FIG_HEIGHT),
            idle_timeout: cli_idle_timeout
                .or(toml.idle_timeout)
                .unwrap_or(DEFAULT_IDLE_TIMEOUT),
            prestart_all_languages: toml
                .prestart_all_languages
                .unwrap_or(DEFAULT_PRESTART_ALL_LANGUAGES),
            style: toml.style.unwrap_or_default(),
        })
    }

    /// Load config with defaults only.
    pub fn load_defaults() -> Result<Self> {
        Self::load(None, None, None)
    }
}

fn load_toml() -> Result<TomlConfig> {
    let path = Path::new(CONFIG_FILE);
    if !path.exists() {
        return Ok(TomlConfig::default());
    }
    let content = std::fs::read_to_string(path)?;
    let config: TomlConfig = toml::from_str(&content)?;
    log::debug!("Loaded config from {CONFIG_FILE}");
    Ok(config)
}
