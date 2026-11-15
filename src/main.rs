mod config;
mod env;
mod kernel;
mod live;
mod options;
mod plan;
mod runner;
mod store;
mod syntax;
#[cfg(test)]
mod typst_tests;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::{Config, Project};
use kernel::{Kernels, ProjectLock};
use live::Live;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use store::Store;

/// Execute Julia, R, and Python code cells in Typst documents.
///
/// Code cells are fenced raw blocks tagged `julia`, `r`, or `python`, and
/// inline code written as `{julia} expr`. loom runs them in persistent
/// kernels, stores the results under `.loom/`, and the Typst library
/// `.loom/loom.typ` (enabled with `#show: loom`) renders them.
#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    /// Show progress details.
    #[arg(long, short, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write `.loom/loom.typ`, an empty `.loom/cells.json`, and `loom.toml`.
    Init {
        /// Project directory.
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Execute stale cells and update `.loom/cells.json`.
    Run {
        /// Root Typst file.
        root: PathBuf,
        /// Execute every cell, ignoring stored results.
        #[arg(long, short)]
        force: bool,
    },
    /// Run, then compile with `typst compile --input loom=strict`.
    Render {
        /// Root Typst file.
        root: PathBuf,
        /// Further arguments for `typst compile` (after `--`).
        #[arg(last = true)]
        typst_args: Vec<String>,
    },
    /// Re-run stale cells whenever a source file is saved.
    Watch {
        /// Root Typst file.
        root: PathBuf,
    },
    /// List chapters, sessions, and the state of their cells.
    List {
        /// Root Typst file.
        root: PathBuf,
    },
    /// Remove stored results and figures the document no longer uses.
    Clean {
        /// Root Typst file.
        root: PathBuf,
    },
    /// Show or stop kernels started by loom in a project.
    Kernels {
        #[command(subcommand)]
        action: KernelAction,
        /// Project directory.
        #[arg(long, default_value = ".", global = true)]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum KernelAction {
    List,
    Stop,
}

const LOOM_TYP: &str = include_str!("../typst/loom.typ");
const LOOM_TOML: &str = include_str!("../templates/loom.toml");
const LOOM_GITIGNORE: &str = "# Runtime state of loom processes.\nkernels/\nlock\n";

fn main() -> Result<()> {
    let cli = Cli::parse();
    let level = if cli.verbose { "info" } else { "warn" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level))
        .format_timestamp(None)
        .init();

    match cli.command {
        Command::Init { dir } => init(&dir),
        Command::Run { root, force } => {
            let project = Project::from_root(&root)?;
            let _lock = ProjectLock::acquire(&project)?;
            install_signal_handler(None)?;
            let cfg = Config::load(&project)?;
            let mut store = Store::load(&project)?;
            let mut kernels = Kernels::default();
            let result = run_once(&project, &cfg, &mut store, &mut kernels, force, None);
            kernels.shutdown();
            result.map(|_| ())
        }
        Command::Render { root, typst_args } => {
            let project = Project::from_root(&root)?;
            let cfg = Config::load(&project)?;
            {
                let _lock = ProjectLock::acquire(&project)?;
                install_signal_handler(None)?;
                let mut store = Store::load(&project)?;
                let mut kernels = Kernels::default();
                let result = run_once(&project, &cfg, &mut store, &mut kernels, false, None);
                kernels.shutdown();
                result?;
            }
            let status = std::process::Command::new(cfg.typst.command())
                .arg("compile")
                .args(["--input", "loom=strict"])
                .arg(&root)
                .args(&typst_args)
                .status()
                .with_context(|| format!("cannot run `{}`", cfg.typst.command()))?;
            if !status.success() {
                bail!("typst compile failed ({status})");
            }
            Ok(())
        }
        Command::Watch { root } => watch(&root),
        Command::List { root } => list(&root),
        Command::Clean { root } => {
            let project = Project::from_root(&root)?;
            let _lock = ProjectLock::acquire(&project)?;
            let cfg = Config::load(&project)?;
            let doc = syntax::parse_document(&project)?;
            let plan = plan::build_plan(&doc, &cfg.execute)?;
            let mut store = Store::load(&project)?;
            let (entries, figs) = store.prune(&project, &plan, 0)?;
            store.save(&project)?;
            store::write_cells_json(&project, &doc, &plan, &store)?;
            println!("Removed {entries} stored results and {figs} figures.");
            Ok(())
        }
        Command::Kernels { action, dir } => {
            let dir = dir.canonicalize()?;
            let project = Project {
                root_file: dir.join("(none)"),
                dir,
            };
            let regs = kernel::registered(&project);
            let live: Vec<_> = regs.iter().filter(|(_, pid, _)| kernel::pid_alive(*pid)).collect();
            match action {
                KernelAction::List => {
                    if live.is_empty() {
                        println!("No running kernels.");
                    }
                    for (lang, pid, loom_pid) in live {
                        println!("{lang:<8} pid {pid:<8} started by loom pid {loom_pid}");
                    }
                }
                KernelAction::Stop => {
                    for (lang, pid, _) in live {
                        unsafe { libc::kill(*pid as libc::pid_t, libc::SIGTERM) };
                        println!("Stopped {lang} kernel (pid {pid}).");
                    }
                }
            }
            Ok(())
        }
    }
}

/// Write the support files every command needs. `loom.typ` is rewritten each
/// time so that it always matches the running binary.
fn install_support(loom_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(loom_dir)?;
    let typ = loom_dir.join("loom.typ");
    if std::fs::read_to_string(&typ).ok().as_deref() != Some(LOOM_TYP) {
        store::write_atomic(&typ, LOOM_TYP.as_bytes())?;
    }
    let cells = loom_dir.join("cells.json");
    if !cells.exists() {
        store::write_atomic(&cells, br#"{"version":1,"cells":{}}"#)?;
    }
    let ignore = loom_dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, LOOM_GITIGNORE)?;
    }
    Ok(())
}

fn init(dir: &Path) -> Result<()> {
    let dir = dir
        .canonicalize()
        .with_context(|| format!("cannot find directory {}", dir.display()))?;
    install_support(&dir.join(".loom"))?;
    let toml = dir.join(config::CONFIG_FILE);
    if !toml.exists() {
        std::fs::write(&toml, LOOM_TOML)?;
        println!("Wrote {}", config::CONFIG_FILE);
    }
    println!("Wrote .loom/loom.typ and .loom/cells.json.");
    println!();
    println!("Enable loom in the root Typst file with:");
    println!();
    println!("  #import \".loom/loom.typ\": loom");
    println!("  #show: loom");
    Ok(())
}

/// Parse, plan, execute stale cells, and write the data file. The data file
/// and the store are written even when a cell fails, so the failure is
/// visible in the document.
fn run_once(
    project: &Project,
    cfg: &Config,
    store: &mut Store,
    kernels: &mut Kernels,
    force: bool,
    live: Option<&Live>,
) -> Result<runner::RunReport> {
    install_support(&project.loom_dir())?;
    let doc = syntax::parse_document(project)?;
    let plan = plan::build_plan(&doc, &cfg.execute)?;
    if let Some(live) = live {
        *live.keys.lock().expect("lock") = plan_keys(&plan);
        live.abort.store(false, Ordering::SeqCst);
    }
    // While watching, the data file is rewritten after every cell so that
    // the preview follows a long run.
    let (doc_ref, plan_ref) = (&doc, &plan);
    let mut on_cell = move |st: &Store| -> Result<()> {
        if live.is_some() {
            store::write_cells_json(project, doc_ref, plan_ref, st)?;
        }
        Ok(())
    };
    let mut hooks = runner::Hooks {
        on_cell: &mut on_cell,
        live,
    };
    let result = runner::run(project, cfg, &doc, &plan, store, kernels, force, &mut hooks);
    store.touch(&plan);
    store.remember_labels(&doc, &plan);
    store.prune(project, &plan, store::KEEP_UNREFERENCED)?;
    store.save(project)?;
    store::write_cells_json(project, &doc, &plan, store)?;
    let report = result?;
    let total = report.executed + report.reused;
    if report.aborted {
        println!("Stopped after {} cell{}.", report.executed, if report.executed == 1 { "" } else { "s" });
    } else if report.executed == 0 {
        println!("All {total} cells up to date.");
    } else {
        println!(
            "Executed {} cell{}, reused {}.",
            report.executed,
            if report.executed == 1 { "" } else { "s" },
            report.reused
        );
    }
    Ok(report)
}

fn plan_keys(plan: &plan::Plan) -> HashSet<String> {
    plan.cell_keys.iter().flatten().cloned().collect()
}

/// Cache keys of the document as it is on disk now; `None` if it cannot be
/// parsed (for example while an option is being typed).
fn current_keys(project: &Project) -> Option<HashSet<String>> {
    let cfg = Config::load(project).ok()?;
    let doc = syntax::parse_document(project).ok()?;
    Some(plan_keys(&plan::build_plan(&doc, &cfg.execute).ok()?))
}

/// Ctrl-C, SIGTERM, and SIGHUP stop loom and its kernels. During `watch`, a
/// Ctrl-C while a cell computes interrupts that cell instead; a second one
/// within two seconds quits.
fn install_signal_handler(live: Option<Arc<Live>>) -> Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    std::thread::spawn(move || {
        for sig in signals.forever() {
            if sig == SIGINT {
                if let Some(live) = &live {
                    if !live.interrupted_within(Duration::from_secs(2)) && live.interrupt_running() {
                        eprintln!("\nInterrupting the running cell; press Ctrl-C again to quit.");
                        continue;
                    }
                }
            }
            kernel::kill_all_and_exit(128 + sig);
        }
    });
    Ok(())
}

fn watch(root: &Path) -> Result<()> {
    use notify::{RecursiveMode, Watcher};

    let project = Project::from_root(root)?;
    let _lock = ProjectLock::acquire(&project)?;
    let mut store = Store::load(&project)?;
    let mut kernels = Kernels::default();
    let live = Arc::new(Live::default());
    install_signal_handler(Some(live.clone()))?;

    let loom_dir = project.loom_dir();
    let relevant = move |p: &Path| {
        !p.starts_with(&loom_dir)
            && (p.extension().and_then(|e| e.to_str()) == Some("typ")
                || p.file_name().and_then(|n| n.to_str()) == Some(config::CONFIG_FILE))
    };
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let watcher_live = live.clone();
    let watcher_project = project.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            if event.paths.iter().any(|p| relevant(p)) {
                watcher_live.source_changed(current_keys(&watcher_project));
                let _ = tx.send(());
            }
        }
    })?;
    watcher.watch(&project.dir, RecursiveMode::Recursive)?;

    println!(
        "Watching {} — Ctrl-C interrupts a running cell, twice quits.",
        project.display_path(&project.root_file)
    );
    loop {
        live.dirty.store(false, Ordering::SeqCst);
        match Config::load(&project) {
            Ok(cfg) => {
                if let Err(e) = run_once(&project, &cfg, &mut store, &mut kernels, false, Some(&live)) {
                    eprintln!("Error: {e:#}");
                }
            }
            Err(e) => eprintln!("Error: {e:#}"),
        }
        // Wait for a change unless one arrived during the run, then let a
        // burst of saves settle.
        if !live.dirty.load(Ordering::SeqCst) {
            rx.recv().context("file watcher stopped")?;
        }
        while rx.recv_timeout(Duration::from_millis(200)).is_ok() {}
    }
}

fn list(root: &Path) -> Result<()> {
    let project = Project::from_root(root)?;
    let cfg = Config::load(&project)?;
    let doc = syntax::parse_document(&project)?;
    let plan = plan::build_plan(&doc, &cfg.execute)?;
    let store = Store::load(&project)?;
    println!(
        "{:<8} {:<24} {:>6} {:>6} {:>6}",
        "LANG", "SESSION", "CELLS", "FRESH", "ERROR"
    );
    for s in &plan.sessions {
        // Cells after the first one without a usable result will run again.
        let fresh = s
            .keys
            .iter()
            .take_while(|k| store.get(k).is_some_and(store::Entry::is_good))
            .count();
        let errors = s
            .keys
            .iter()
            .filter(|k| store.get(k).is_some_and(|e| !e.is_good()))
            .count();
        println!(
            "{:<8} {:<24} {:>6} {:>6} {:>6}",
            s.lang.tag(),
            s.name,
            s.cells.len(),
            fresh,
            errors
        );
    }
    let skipped = plan.cell_keys.iter().filter(|k| k.is_none()).count();
    if skipped > 0 {
        println!("{skipped} cell(s) with `eval: false`.");
    }
    Ok(())
}
