//! Executing stale cells.
//!
//! A stored result is reusable when it exists, is not an unexpected failure,
//! and was computed in the current package environment (see `env`). For each
//! session, the first cell without a reusable result is found. If the
//! kernel's session is in exactly the state that cell expects (its position
//! equals the key of the state before the cell), execution continues from
//! there. Otherwise the session is reset and every cell of the session runs
//! again from the start, refreshing the stored results of the cells before
//! the stale one as well; this keeps all outputs of a session the product of
//! one uninterrupted execution, even when code is not deterministic.
//!
//! Each language runs on its own thread, executing its cells in document
//! order. A cell that imports a value waits until the cell exporting it has
//! a result. Exports always come earlier in the document than the imports
//! that read them, so the earliest unfinished cell can always run and the
//! threads cannot wait on each other in a cycle.
//!
//! A cell marked `stateless` runs without moving the session's position, and
//! fails if it created or rebound a name.

use crate::config::{Config, Project};
use crate::env::EnvChecker;
use crate::kernel::{ExecRequest, Kernel, Kernels};
use crate::live::{Live, Running};
use crate::plan::{Plan, SessionPlan};
use crate::store::{self, Entry, Store};
use crate::syntax::{Cell, Document, Lang};
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Condvar, Mutex};

#[derive(Debug, Default)]
pub struct RunReport {
    pub executed: usize,
    pub reused: usize,
    /// The run stopped early: sources changed, or a cell was interrupted.
    pub aborted: bool,
}

/// Callbacks of a run.
pub struct Hooks<'a> {
    /// Called after each executed cell, with the updated store.
    pub on_cell: &'a mut (dyn FnMut(&Store) -> Result<()> + Send),
    /// Shared with the file watcher and the signal handler, during `watch`.
    pub live: Option<&'a Live>,
}

/// One cell to execute.
struct Job {
    /// Index into `Document::cells`.
    idx: usize,
    /// Index into `Plan::sessions`.
    si: usize,
    /// Index of the cell within its session.
    j: usize,
    /// Reset the session (replaying setup code) before this cell.
    reset: bool,
}

/// State shared by the language threads.
struct State<'s> {
    store: &'s mut Store,
    on_cell: &'s mut (dyn FnMut(&Store) -> Result<()> + Send),
    /// No further cell starts once set.
    stopped: bool,
    aborted: bool,
    error: Option<anyhow::Error>,
    /// Languages whose thread has finished.
    done: HashSet<Lang>,
    executed: usize,
}

struct Ctx<'a> {
    project: &'a Project,
    cfg: &'a Config,
    doc: &'a Document,
    plan: &'a Plan,
    live: Option<&'a Live>,
    cond: Condvar,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    project: &Project,
    cfg: &Config,
    doc: &Document,
    plan: &Plan,
    store: &mut Store,
    kernels: &mut Kernels,
    force: bool,
    hooks: &mut Hooks,
) -> Result<RunReport> {
    let mut report = RunReport::default();
    let mut checker = EnvChecker::new(project, cfg);
    let mut restarted: HashSet<Lang> = HashSet::new();
    let mut jobs: HashMap<Lang, Vec<Job>> = HashMap::new();
    for (si, s) in plan.sessions.iter().enumerate() {
        let n = s.cells.len();
        let (first_stale, env_change) = if force {
            (Some(0), None)
        } else {
            find_stale(s, store, &mut checker)?
        };
        let Some(stale) = first_stale else {
            report.reused += n;
            continue;
        };
        if let Some(why) = env_change {
            // A process cannot replace a package it has loaded.
            if restarted.insert(s.lang) && kernels.is_running(s.lang) {
                eprintln!("{}: {why}; restarting the kernel", s.lang.name());
                kernels.discard(s.lang);
            }
        }
        let resume = !force && kernels.position(s.lang, &s.name) == Some(s.key_before(stale));
        let start = if resume { stale } else { 0 };
        eprintln!(
            "{} · {}: running {} of {} cell{}",
            s.lang.name(),
            s.name,
            n - start,
            n,
            if n == 1 { "" } else { "s" }
        );
        report.reused += start;
        let queue = jobs.entry(s.lang).or_default();
        for j in start..n {
            queue.push(Job {
                idx: s.cells[j],
                si,
                j,
                reset: j == start && !resume,
            });
        }
    }
    if jobs.is_empty() {
        return Ok(report);
    }
    for queue in jobs.values_mut() {
        queue.sort_by_key(|job| job.idx);
    }

    let ctx = Ctx {
        project,
        cfg,
        doc,
        plan,
        live: hooks.live,
        cond: Condvar::new(),
    };
    let state = Mutex::new(State {
        store,
        on_cell: &mut *hooks.on_cell,
        stopped: false,
        aborted: false,
        error: None,
        done: HashSet::new(),
        executed: 0,
    });
    let returned: Vec<Kernel> = std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|(lang, queue)| {
                let kernel = kernels.take(lang);
                let (ctx, state) = (&ctx, &state);
                scope.spawn(move || run_language(ctx, state, lang, queue, kernel))
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().expect("language thread panicked"))
            .collect()
    });
    for k in returned {
        kernels.put(k);
    }
    let st = state.into_inner().expect("state lock");
    report.executed = st.executed;
    report.aborted = st.aborted;
    match st.error {
        Some(e) => Err(e),
        None => Ok(report),
    }
}

/// Execute one language's cells. Returns the kernel, unless it was lost.
fn run_language(
    ctx: &Ctx,
    state: &Mutex<State>,
    lang: Lang,
    queue: Vec<Job>,
    mut kernel: Option<Kernel>,
) -> Option<Kernel> {
    let mut checker = EnvChecker::new(ctx.project, ctx.cfg);
    for job in queue {
        if ctx.live.is_some_and(|l| l.abort.load(Ordering::SeqCst)) {
            stop(ctx, state, None, true);
            break;
        }
        let Some(imports) = wait_for_imports(ctx, state, lang, &job) else {
            break;
        };
        if kernel.is_none() {
            eprintln!("Starting {} kernel…", lang.name());
            match Kernel::start(lang, ctx.cfg.lang(lang), ctx.project) {
                Ok(k) => kernel = Some(k),
                Err(e) => {
                    stop(ctx, state, Some(e), false);
                    break;
                }
            }
        }
        let k = kernel.as_mut().expect("kernel started");
        match run_job(ctx, state, k, &mut checker, &job, imports) {
            Ok(Outcome::Done) => {}
            Ok(Outcome::Stop) => break,
            Err(e) => {
                // The kernel may be in any state after a transport failure.
                kernel = None;
                stop(ctx, state, Some(e), false);
                break;
            }
        }
    }
    let mut st = state.lock().expect("state lock");
    st.done.insert(lang);
    ctx.cond.notify_all();
    kernel
}

/// Record that the run stops: no further cell starts.
fn stop(ctx: &Ctx, state: &Mutex<State>, error: Option<anyhow::Error>, aborted: bool) {
    let mut st = state.lock().expect("state lock");
    st.stopped = true;
    st.aborted |= aborted;
    if let Some(e) = error {
        st.error.get_or_insert(e);
    }
    ctx.cond.notify_all();
}

/// Wait until every value the job imports has been exported, and return the
/// files; `None` when the run stopped in the meantime.
fn wait_for_imports(
    ctx: &Ctx,
    state: &Mutex<State>,
    lang: Lang,
    job: &Job,
) -> Option<Vec<(String, String)>> {
    let deps = &ctx.plan.imports[job.idx];
    let mut st = state.lock().expect("state lock");
    loop {
        if st.stopped {
            return None;
        }
        let mut files = Vec::new();
        let mut missing = None;
        for (name, e) in deps {
            let key = ctx.plan.cell_keys[*e].as_deref().expect("exporter is evaluated");
            match st
                .store
                .get(key)
                .filter(|entry| entry.is_good())
                .and_then(|entry| entry.exports.get(name))
            {
                Some(rel) => files.push((
                    name.clone(),
                    ctx.project.loom_dir().join(rel).display().to_string(),
                )),
                None => {
                    missing = Some((name, *e));
                    break;
                }
            }
        }
        let Some((name, e)) = missing else {
            return Some(files);
        };
        // The exporter is either still queued on another language's thread,
        // or it will never produce the value: cells of this language that
        // come earlier in the document have already run.
        let exporter_lang = ctx.doc.cells[e].lang;
        if exporter_lang == lang || st.done.contains(&exporter_lang) {
            let cell = &ctx.doc.cells[job.idx];
            let err = anyhow!(
                "the {} cell at {} imports `{name}`, but the cell exporting it, at {}, has no result",
                cell.lang.name(),
                cell.location(ctx.project),
                ctx.doc.cells[e].location(ctx.project)
            );
            st.stopped = true;
            st.error.get_or_insert(err);
            ctx.cond.notify_all();
            return None;
        }
        st = ctx.cond.wait(st).expect("state lock");
    }
}

enum Outcome {
    Done,
    Stop,
}

fn run_job(
    ctx: &Ctx,
    state: &Mutex<State>,
    kernel: &mut Kernel,
    checker: &mut EnvChecker,
    job: &Job,
    imports: Vec<(String, String)>,
) -> Result<Outcome> {
    let (project, plan, doc) = (ctx.project, ctx.plan, ctx.doc);
    let s = &plan.sessions[job.si];
    let cell = &doc.cells[job.idx];
    let exec = plan.exec[job.idx].as_ref().expect("evaluated cell");
    if job.reset {
        let setup = if s.is_setup() {
            Vec::new()
        } else {
            plan.setup_code(doc, s.lang)
        };
        let reply = kernel.reset(&s.name, &s.key0, &setup)?;
        if reply.failed {
            stop(
                ctx,
                state,
                Some(anyhow!(
                    "setup code failed while preparing the {} session `{}`:\n{}",
                    s.lang.name(),
                    s.name,
                    error_text(&reply.outputs)
                )),
                false,
            );
            return Ok(Outcome::Stop);
        }
    }
    let key = &s.keys[job.j];
    log::info!("running {}", cell.location(project));
    let mut options = exec.kernel_options();
    if s.lang == Lang::Python {
        if let Some(d) = &ctx.cfg.python.dataframe {
            options["dataframe"] = json!(d);
        }
    }
    if let Some(live) = ctx.live {
        live.running.lock().expect("lock").push(Running {
            pid: kernel.pid(),
            key: key.clone(),
        });
    }
    let file = project.display_path(&cell.file);
    let reply = kernel.exec(ExecRequest {
        session: &s.name,
        after: s.key_before(job.j),
        id: s.key_after(job.j),
        inline: cell.inline,
        code: &cell.code,
        options,
        file: &file,
        line: cell.code_line,
        imports,
        exports: &cell.options.exports,
    });
    if let Some(live) = ctx.live {
        live.running.lock().expect("lock").retain(|r| &r.key != key);
    }
    let mut reply = reply?;
    if ctx.live.is_some_and(|l| l.take_interrupted(key)) {
        // An interrupted cell left the session in an unknown state, and its
        // partial output is not a result.
        kernel.forget(&s.name);
        eprintln!("{}: interrupted {}", s.lang.name(), cell.location(project));
        stop(ctx, state, None, true);
        return Ok(Outcome::Stop);
    }
    store::import_figures(project, &mut reply.outputs)?;
    let exports = store::import_shared(project, &reply.exports)?;
    let mut failed = reply.failed;
    let mut message = if failed {
        cell_failure(project, cell, &error_text(&reply.outputs))
    } else {
        String::new()
    };
    if cell.is_stateless() && !reply.changed.is_empty() {
        // The session no longer matches its position.
        kernel.forget(&s.name);
        let text = format!(
            "loom: this cell is marked `stateless` but changed {}; remove `stateless: true` or the assignment",
            reply.changed.join(", ")
        );
        message = format!("{} cell at {}: {text}", cell.lang.name(), cell.location(project));
        reply.outputs.push(json!({ "type": "error", "text": text }));
        failed = true;
    }
    let env_record = checker.record(s.lang, &reply.packages)?;
    let error_ok = exec.error && !cell.is_stateless();

    let mut st = state.lock().expect("state lock");
    if let Some(old) = st.store.get(key) {
        for (name, file) in &exports {
            if old.exports.get(name).is_some_and(|f| f != file) {
                eprintln!(
                    "Warning: the {} cell at {} exported a different `{name}` than when it last \
                     ran, although its code did not change; cells that imported the earlier value \
                     were not rerun. Make the cell reproducible (for example, set a random seed).",
                    cell.lang.name(),
                    cell.location(project)
                );
            }
        }
    }
    let env = st.store.insert_env(env_record);
    st.store.insert(
        key.clone(),
        Entry {
            lang: s.lang.tag().to_string(),
            session: s.name.clone(),
            failed,
            error_ok,
            outputs: reply.outputs,
            exec: cell.options.exec_raw(),
            fig_width: exec.fig_width,
            fig_height: exec.fig_height,
            used: store::now(),
            env: Some(env),
            exports,
        },
    );
    st.executed += 1;
    let State { store, on_cell, .. } = &mut *st;
    let written = on_cell(store);
    ctx.cond.notify_all();
    drop(st);
    written?;
    if failed && !error_ok {
        stop(ctx, state, Some(anyhow!("{message}")), false);
        return Ok(Outcome::Stop);
    }
    Ok(Outcome::Done)
}

/// Index of the first cell of `s` without a reusable result, and, when the
/// reason is a changed package environment, a description of the change.
fn find_stale(
    s: &SessionPlan,
    store: &Store,
    checker: &mut EnvChecker,
) -> Result<(Option<usize>, Option<String>)> {
    for (i, key) in s.keys.iter().enumerate() {
        let Some(entry) = store.get(key) else {
            return Ok((Some(i), None));
        };
        if !entry.is_good() {
            return Ok((Some(i), None));
        }
        let Some(id) = &entry.env else { continue };
        let Some(record) = store.env(id) else {
            return Ok((Some(i), None));
        };
        if let Some(m) = checker.check(s.lang, id, record)? {
            return Ok((Some(i), Some(m.0)));
        }
    }
    Ok((None, None))
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
