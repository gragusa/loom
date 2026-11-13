//! Cell options written as `#| key: value` lines at the top of a code cell.
//!
//! The Typst library (`typst/loom.typ`) splits the option lines from the code
//! with the same rule as `split_options`; the cell lookup key depends on both
//! sides agreeing on where the code starts.

use crate::config::FigFormat;
use std::collections::BTreeMap;

/// Options that change what a kernel computes. They enter the cache key, so
/// editing one re-executes the cell. All other options only change how Typst
/// draws the stored result.
pub const EXEC_KEYS: &[&str] = &[
    "session",
    "setup",
    "error",
    "fig-width",
    "fig-height",
    "fig-format",
    "fig-dpi",
];

const RENDER_KEYS: &[&str] = &[
    "echo", "output", "include", "warning", "message", "prompt", "collapse", "label", "fig-cap",
    "tbl-cap", "out-width", "column", "comment",
];

/// Session name reserved for setup cells.
pub const SETUP_SESSION: &str = "setup";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CellOptions {
    /// Every option as written (value with surrounding whitespace removed).
    pub raw: BTreeMap<String, String>,
    pub eval: bool,
    pub session: Option<String>,
    pub setup: Option<bool>,
    pub error: Option<bool>,
    pub fig_width: Option<f64>,
    pub fig_height: Option<f64>,
    pub fig_format: Option<FigFormat>,
    pub fig_dpi: Option<f64>,
}

impl CellOptions {
    /// Execution options as written, for the cache key and for the Typst
    /// library's check that a stored result matches the cell on screen.
    pub fn exec_raw(&self) -> BTreeMap<String, String> {
        self.raw
            .iter()
            .filter(|(k, _)| EXEC_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// Split a cell's text into option lines and code. Option lines are the
/// leading lines whose first non-blank characters are `#|`.
pub fn split_options(text: &str) -> (Vec<&str>, String) {
    let lines: Vec<&str> = text.split('\n').collect();
    let n = lines
        .iter()
        .take_while(|l| l.trim_start().starts_with("#|"))
        .count();
    (lines[..n].to_vec(), lines[n..].join("\n"))
}

/// Parse option lines. Errors name the offending option; the caller adds the
/// file and line.
pub fn parse_options(lines: &[&str]) -> Result<CellOptions, String> {
    let mut opts = CellOptions {
        eval: true,
        ..Default::default()
    };
    for line in lines {
        let body = line.trim_start().strip_prefix("#|").unwrap_or_default().trim();
        if body.is_empty() {
            continue;
        }
        let (key, value) = body
            .split_once(':')
            .ok_or_else(|| format!("option line `{}` is not of the form `key: value`", line.trim()))?;
        let key = key.trim();
        let value = value.trim();
        if opts.raw.insert(key.to_string(), value.to_string()).is_some() {
            return Err(format!("option `{key}` is given twice"));
        }
        let text = unquote(value);
        match key {
            "eval" => opts.eval = parse_bool(key, value)?,
            "setup" => opts.setup = Some(parse_bool(key, value)?),
            "error" => opts.error = Some(parse_bool(key, value)?),
            "echo" | "include" | "warning" | "message" | "prompt" | "collapse" => {
                parse_bool(key, value)?;
            }
            "output" => {
                if !matches!(value, "true" | "false" | "asis") {
                    return Err(format!(
                        "option `output` must be true, false, or asis, not `{value}`"
                    ));
                }
            }
            "session" => {
                if text.is_empty() {
                    return Err("option `session` must not be empty".into());
                }
                if text == SETUP_SESSION {
                    return Err(format!(
                        "session name `{SETUP_SESSION}` is reserved; use `#| setup: true`"
                    ));
                }
                opts.session = Some(text.to_string());
            }
            "fig-width" => opts.fig_width = Some(parse_positive(key, value)?),
            "fig-height" => opts.fig_height = Some(parse_positive(key, value)?),
            "fig-dpi" => opts.fig_dpi = Some(parse_positive(key, value)?),
            "fig-format" => {
                opts.fig_format = Some(FigFormat::parse(text).ok_or_else(|| {
                    format!("option `fig-format` must be svg, png, or pdf, not `{value}`")
                })?)
            }
            "label" => check_label(text)?,
            "column" => {
                if !matches!(text, "body" | "margin" | "page") {
                    return Err(format!(
                        "option `column` must be body, margin, or page, not `{value}`"
                    ));
                }
            }
            "fig-cap" | "tbl-cap" | "out-width" | "comment" => {}
            _ => {
                return Err(format!(
                    "unknown option `{key}`; known options: eval, {}, {}",
                    EXEC_KEYS.join(", "),
                    RENDER_KEYS.join(", ")
                ));
            }
        }
    }
    if opts.setup == Some(true) && opts.session.is_some() {
        return Err("a setup cell cannot also name a `session`".into());
    }
    Ok(opts)
}

fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    v
}

fn parse_bool(key: &str, value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("option `{key}` must be true or false, not `{value}`")),
    }
}

fn parse_positive(key: &str, value: &str) -> Result<f64, String> {
    match value.parse::<f64>() {
        Ok(x) if x > 0.0 && x.is_finite() => Ok(x),
        _ => Err(format!("option `{key}` must be a positive number, not `{value}`")),
    }
}

/// Labels become Typst labels on the generated element; the prefix selects
/// the element: `fig-` a figure, `tbl-` a table figure, `eq-` the markup
/// produced by `output: asis` (typically an equation).
fn check_label(label: &str) -> Result<(), String> {
    let valid_chars = !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    if !valid_chars {
        return Err(format!(
            "label `{label}` may contain only letters, digits, and - _ . :"
        ));
    }
    if !["fig-", "tbl-", "eq-"].iter().any(|p| label.starts_with(p)) {
        return Err(format!(
            "label `{label}` must start with fig-, tbl-, or eq-"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_leading_option_lines_only() {
        let (opts, code) = split_options("#| echo: false\n  #| label: fig-a\nx = 1\n#| not an option");
        assert_eq!(opts, vec!["#| echo: false", "  #| label: fig-a"]);
        assert_eq!(code, "x = 1\n#| not an option");
    }

    #[test]
    fn no_options() {
        let (opts, code) = split_options("x = 1\n\ny = 2");
        assert!(opts.is_empty());
        assert_eq!(code, "x = 1\n\ny = 2");
    }

    #[test]
    fn parses_typed_options() {
        let o = parse_options(&[
            "#| fig-width: 6",
            "#| fig-format: \"png\"",
            "#| session: shared",
            "#| fig-cap: A plot of $x^2$: see @fig-b.",
            "#| eval: false",
        ])
        .unwrap();
        assert_eq!(o.fig_width, Some(6.0));
        assert_eq!(o.fig_format, Some(FigFormat::Png));
        assert_eq!(o.session.as_deref(), Some("shared"));
        assert!(!o.eval);
        assert_eq!(o.raw["fig-cap"], "A plot of $x^2$: see @fig-b.");
        let exec = o.exec_raw();
        assert_eq!(exec.keys().collect::<Vec<_>>(), ["fig-format", "fig-width", "session"]);
    }

    #[test]
    fn rejects_bad_options() {
        for bad in [
            "#| echo: yes",
            "#| fig_width: 3",
            "#| fig-width: -1",
            "#| output: maybe",
            "#| label: plot-a",
            "#| label: fig a",
            "#| session: setup",
            "#| column: left",
            "#| justtext",
        ] {
            assert!(parse_options(&[bad]).is_err(), "accepted {bad}");
        }
        assert!(parse_options(&["#| echo: true", "#| echo: false"]).is_err());
        assert!(parse_options(&["#| setup: true", "#| session: a"]).is_err());
    }
}
