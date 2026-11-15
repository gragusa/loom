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
    "stateless",
    "import",
    "export",
    "error",
    "fig-width",
    "fig-height",
    "fig-format",
    "fig-dpi",
];

const RENDER_KEYS: &[&str] = &[
    "echo", "output", "include", "warning", "message", "prompt", "collapse", "label", "fig-cap",
    "tbl-cap", "fig-subcap", "out-width", "column", "layout-ncol", "comment",
];

/// Options whose value may be a list written over several lines:
/// `#| fig-subcap:` followed by `#|   - item` lines.
const LIST_KEYS: &[&str] = &["fig-subcap"];

/// Session name reserved for setup cells.
pub const SETUP_SESSION: &str = "setup";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CellOptions {
    /// Every option as written (value with surrounding whitespace removed).
    pub raw: BTreeMap<String, String>,
    pub eval: bool,
    pub session: Option<String>,
    pub setup: Option<bool>,
    /// The cell declares that it creates and rebinds no variables.
    pub stateless: Option<bool>,
    /// Names whose values are read from earlier exports before the cell runs.
    pub imports: Vec<String>,
    /// Names whose values are written for later cells after the cell runs.
    pub exports: Vec<String>,
    /// Captions of the subfigures, in order.
    pub fig_subcap: Option<Vec<String>>,
    pub error: Option<bool>,
    pub fig_width: Option<f64>,
    pub fig_height: Option<f64>,
    pub fig_format: Option<FigFormat>,
    pub fig_dpi: Option<f64>,
}

impl CellOptions {
    /// The `label` option without surrounding quotes.
    pub fn label(&self) -> Option<&str> {
        self.raw.get("label").map(|v| unquote(v))
    }

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
    // The option whose list items (`- item` lines) are being read.
    let mut list_key: Option<&str> = None;
    for line in lines {
        let body = line.trim_start().strip_prefix("#|").unwrap_or_default().trim();
        if body.is_empty() {
            continue;
        }
        if let Some(item) = body.strip_prefix('-') {
            let Some(key) = list_key else {
                return Err(format!(
                    "list item `{body}` must follow an option written as `{}:` with no value",
                    LIST_KEYS.join(":`, `")
                ));
            };
            let item = item.trim();
            if item.is_empty() {
                return Err(format!("empty list item in option `{key}`"));
            }
            opts.fig_subcap
                .get_or_insert_with(Vec::new)
                .push(unquote(item).to_string());
            continue;
        }
        list_key = None;
        let (key, value) = body
            .split_once(':')
            .ok_or_else(|| format!("option line `{}` is not of the form `key: value`", line.trim()))?;
        let key = key.trim();
        let value = value.trim();
        if opts.raw.insert(key.to_string(), value.to_string()).is_some() {
            return Err(format!("option `{key}` is given twice"));
        }
        let text = unquote(value);
        if value.is_empty() {
            if let Some(&k) = LIST_KEYS.iter().find(|&&k| k == key) {
                list_key = Some(k);
                continue;
            }
        }
        match key {
            "eval" => opts.eval = parse_bool(key, value)?,
            "setup" => opts.setup = Some(parse_bool(key, value)?),
            "stateless" => opts.stateless = Some(parse_bool(key, value)?),
            "import" => opts.imports = parse_names(key, text)?,
            "export" => opts.exports = parse_names(key, text)?,
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
            "layout-ncol" => {
                if !value.parse::<u32>().is_ok_and(|n| n >= 1) {
                    return Err(format!(
                        "option `layout-ncol` must be a positive integer, not `{value}`"
                    ));
                }
            }
            "fig-subcap" => opts.fig_subcap = Some(parse_flow_list(key, value)?),
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
    if let Some(k) = list_key {
        if opts.fig_subcap.is_none() {
            return Err(format!("option `{k}` has no value and no list items"));
        }
    }
    if opts.setup == Some(true) && opts.session.is_some() {
        return Err("a setup cell cannot also name a `session`".into());
    }
    if opts.setup == Some(true) && opts.stateless == Some(true) {
        return Err("a setup cell cannot be `stateless`".into());
    }
    if !opts.imports.is_empty() && opts.setup == Some(true) {
        return Err("a setup cell cannot `import`: setup code is replayed without its imports".into());
    }
    if !opts.imports.is_empty() && opts.stateless == Some(true) {
        return Err("a `stateless` cell cannot `import`: importing binds names".into());
    }
    Ok(opts)
}

/// A list written on one line, as in YAML: `["First", 'Second', third]`.
/// Quoted items end at the matching quote (there are no escapes); unquoted
/// items end at the next comma. The Typst library parses lists the same way.
fn parse_flow_list(key: &str, value: &str) -> Result<Vec<String>, String> {
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .ok_or_else(|| {
            format!("option `{key}` must be a list: `[\"first\", \"second\"]`, or `- item` lines")
        })?;
    let chars: Vec<char> = inner.chars().collect();
    let mut items = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let c = chars[i];
        if c == '"' || c == '\'' {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j] != c {
                j += 1;
            }
            if j >= chars.len() {
                return Err(format!("option `{key}`: unterminated quote"));
            }
            items.push(chars[start..j].iter().collect());
            i = j + 1;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if i < chars.len() {
                if chars[i] != ',' {
                    return Err(format!("option `{key}`: expected `,` after a quoted item"));
                }
                i += 1;
            }
        } else {
            let start = i;
            while i < chars.len() && chars[i] != ',' {
                i += 1;
            }
            let item: String = chars[start..i].iter().collect::<String>().trim().to_string();
            if item.is_empty() {
                return Err(format!("option `{key}`: empty item"));
            }
            items.push(item);
            i += 1;
        }
    }
    if items.is_empty() {
        return Err(format!("option `{key}` needs at least one item"));
    }
    Ok(items)
}

/// A comma-separated list of names, as `import: wages, n`. The Typst library
/// splits the list with the same rule.
fn parse_names(key: &str, text: &str) -> Result<Vec<String>, String> {
    let names: Vec<String> = text
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        return Err(format!("option `{key}` needs at least one name"));
    }
    for n in &names {
        let mut chars = n.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '.');
        if !first_ok || !chars.all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
            return Err(format!(
                "option `{key}`: `{n}` is not a variable name (letters, digits, _ and .)"
            ));
        }
    }
    let mut seen = std::collections::HashSet::new();
    if let Some(dup) = names.iter().find(|n| !seen.insert(n.as_str())) {
        return Err(format!("option `{key}` lists `{dup}` twice"));
    }
    Ok(names)
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
    fn parses_subcaption_lists() {
        let o = parse_options(&[
            "#| fig-subcap:",
            "#|   - \"Levels, in logs\"",
            "#|   - Changes",
            "#| layout-ncol: 2",
        ])
        .unwrap();
        assert_eq!(o.fig_subcap.unwrap(), ["Levels, in logs", "Changes"]);
        let o = parse_options(&["#| fig-subcap: [\"A, b\", 'C' , d e]"]).unwrap();
        assert_eq!(o.fig_subcap.unwrap(), ["A, b", "C", "d e"]);
        for bad in [
            vec!["#| - stray"],
            vec!["#| fig-subcap:"],
            vec!["#| fig-subcap: A, B"],
            vec!["#| fig-subcap: [\"A]"],
            vec!["#| fig-subcap: []"],
            vec!["#| echo: true", "#|   - x"],
        ] {
            assert!(parse_options(&bad).is_err(), "accepted {bad:?}");
        }
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
            "#| import: 1x",
            "#| export:",
            "#| import: a, a",
            "#| session: setup",
            "#| column: left",
            "#| justtext",
        ] {
            assert!(parse_options(&[bad]).is_err(), "accepted {bad}");
        }
        assert!(parse_options(&["#| echo: true", "#| echo: false"]).is_err());
        assert!(parse_options(&["#| setup: true", "#| session: a"]).is_err());
        assert!(parse_options(&["#| setup: true", "#| import: a"]).is_err());
        assert!(parse_options(&["#| stateless: true", "#| import: a"]).is_err());
        let o = parse_options(&["#| import: wages, n_obs", "#| export: fit.table"]).unwrap();
        assert_eq!(o.imports, ["wages", "n_obs"]);
        assert_eq!(o.exports, ["fit.table"]);
    }
}
