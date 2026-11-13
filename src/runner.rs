//! Executing stale cells.
//!
//! For each session, the first cell without a reusable stored result is
//! found. If the kernel's session is in exactly the state that cell expects
//! (its position equals the key of the previous cell), execution continues
//! from there. Otherwise the session is reset and every cell of the session
//! runs again from the start, refreshing the stored results of the cells
//! before the stale one as well; this keeps all outputs of a session the
//! product of one uninterrupted execution, even when code is not
//! deterministic.

use crate::config::{Config, Project};
use crate::kernel::Kernels;
use crate::plan::Plan;
use crate::store::{self, Entry, Store};
use crate::syntax::{Cell, Document};
use anyhow::{Result, bail};
use serde_json::Value;

#[derive(Debug, Default)]
pub struct RunReport {
    pub executed: usize,
    pub reused: usize,
}

pub fn run(
    project: &Project,
    cfg: &Config,
    doc: &Document,
    plan: &Plan,
    store: &mut Store,
    kernels: &mut Kernels,
    force: bool,
) -> Result<RunReport> {
    let mut report = RunReport::default();
    for s in &plan.sessions {
        let n = s.cells.len();
        let first_stale = if force {
            Some(0)
        } else {
            (0..n).find(|&i| !store.get(&s.keys[i]).is_some_and(Entry::is_good))
        };
        let Some(stale) = first_stale else {
            report.reused += n;
            continue;
        };
        let kernel = kernels.get(s.lang, cfg.lang(s.lang))?;
        let start = if !force && kernel.position(&s.name) == Some(s.key_before(stale)) {
            stale
        } else {
            let setup = if s.is_setup() {
                Vec::new()
            } else {
                plan.setup_code(doc, s.lang)
            };
            let reply = match kernel.reset(&s.name, &s.key0, &setup) {
                Ok(r) => r,
                Err(e) => {
                    kernels.discard(s.lang);
                    return Err(e);
                }
            };
            if reply.failed {
                bail!(
                    "setup code failed while preparing the {} session `{}`:\n{}",
                    s.lang.name(),
                    s.name,
                    error_text(&reply.outputs)
                );
            }
            0
        };
        eprintln!(
            "{} · {}: running {} of {} cell{}",
            s.lang.name(),
            s.name,
            n - start,
            n,
            if n == 1 { "" } else { "s" }
        );
        report.reused += start;
        for j in start..n {
            let idx = s.cells[j];
            let cell = &doc.cells[idx];
            let exec = plan.exec[idx].as_ref().expect("evaluated cell");
            log::info!("running {}", cell.location(project));
            let kernel = kernels.get(s.lang, cfg.lang(s.lang))?;
            let reply = kernel.exec(
                &s.name,
                s.key_before(j),
                &s.keys[j],
                cell.inline,
                &cell.code,
                exec.kernel_options(),
                &project.display_path(&cell.file),
                cell.code_line,
            );
            let mut reply = match reply {
                Ok(r) => r,
                Err(e) => {
                    kernels.discard(s.lang);
                    return Err(e);
                }
            };
            store::import_figures(project, &mut reply.outputs)?;
            let failed = reply.failed;
            let message = if failed { error_text(&reply.outputs) } else { String::new() };
            store.insert(
                s.keys[j].clone(),
                Entry {
                    lang: s.lang.tag().to_string(),
                    session: s.name.clone(),
                    failed,
                    error_ok: exec.error,
                    outputs: reply.outputs,
                    exec: cell.options.exec_raw(),
                    fig_width: exec.fig_width,
                    fig_height: exec.fig_height,
                    used: store::now(),
                },
            );
            report.executed += 1;
            if failed && !exec.error {
                bail!("{}", cell_failure(project, cell, &message));
            }
        }
    }
    Ok(report)
}

fn cell_failure(project: &Project, cell: &Cell, message: &str) -> String {
    format!(
        "{} cell at {} raised an error (add `#| error: true` if the error is intended):\n{}",
        cell.lang.name(),
        cell.location(project),
        indent(message)
    )
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn error_text(outputs: &[Value]) -> String {
    outputs
        .iter()
        .filter(|o| o.get("type").and_then(Value::as_str) == Some("error"))
        .filter_map(|o| o.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}
