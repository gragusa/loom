mod cache;
mod codegen;
mod config;
mod daemon;
mod parser;
mod runner;
mod watcher;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// loom — code preprocessor for Typst books
///
/// Weaves Julia and R code output into Typst documents. Finds code blocks
/// (`#jlrun`, `#rrun`, `#jlconsole`, `#rconsole`, etc.), executes them via
/// persistent daemons, caches results, and writes the output for Typst.
#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize loom in the current directory: writes `.loom/julia.typ`,
    /// `.loom/r.typ`, `.loom/loom.typ`, and `loom.toml` if they don't already exist.
    Init,

    Run {
        /// Root Typst file (e.g. `document.typ`).
        root: PathBuf,

        /// Re-run only this chapter (e.g. `--chapter introduction`).
        #[arg(long, short)]
        chapter: Option<String>,

        /// Directory for cached outputs.
        #[arg(long)]
        cache_dir: Option<PathBuf>,

        /// Force re-execution of all cells (ignore hash cache).
        #[arg(long, short)]
        force: bool,

        /// TCP port for the Julia daemon.
        #[arg(long)]
        port: Option<u16>,

        /// Daemon idle timeout in seconds (0 = no timeout). Default: 1800 (30 min).
        #[arg(long)]
        idle_timeout: Option<u64>,

        /// Show detailed progress (daemon startup, per-chapter status, file writes).
        #[arg(long, short)]
        verbose: bool,
    },

    /// Watch Typst sources; re-run only the chapter(s) that changed.
    Watch {
        /// Root Typst file.
        root: PathBuf,

        /// Directory for cached outputs.
        #[arg(long)]
        cache_dir: Option<PathBuf>,

        /// TCP port for the Julia daemon.
        #[arg(long)]
        port: Option<u16>,

        /// Daemon idle timeout in seconds (0 = no timeout). Default: 1800 (30 min).
        #[arg(long)]
        idle_timeout: Option<u64>,

        /// Show detailed progress (daemon startup, per-chapter status, file writes).
        #[arg(long, short)]
        verbose: bool,
    },

    /// List chapters and their cell counts (no execution).
    List {
        /// Root Typst file.
        root: PathBuf,
    },

    /// Manage daemon processes.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Show running loom daemons.
    List,

    /// Stop a running daemon.
    Stop {
        /// Port of the daemon to stop (default: config port).
        #[arg(long)]
        port: Option<u16>,

        /// Stop all running loom daemons.
        #[arg(long)]
        all: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Determine log level: --verbose (or RUST_LOG override) → info; default → warn.
    let verbose = match &cli.command {
        Command::Run { verbose, .. } => *verbose,
        Command::Watch { verbose, .. } => *verbose,
        _ => false,
    };
    let default_level = if verbose { "info" } else { "warn" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_level))
        .init();

    match cli.command {
        Command::Init => {
            init_project()?;
        }

        Command::Run {
            root,
            chapter,
            cache_dir,
            force,
            port,
            idle_timeout,
            verbose: _,
        } => {
            // Auto-write julia.typ and r.typ if they don't exist.
            write_typst_files_if_missing()?;
            let cfg = config::Config::load(port, cache_dir.as_deref(), idle_timeout)?;
            let book = parser::parse_book(&root)?;

            // Spawn daemons for languages actually used in the book.
            let mut daemons_started: Vec<&str> = Vec::new();

            let jl_client =
                if cfg.prestart_all_languages || book.uses_language(parser::Language::Julia) {
                    let client = daemon::DaemonClient::connect_or_spawn(
                        cfg.julia_port,
                        &cfg.julia,
                        parser::Language::Julia,
                        cfg.idle_timeout,
                    )
                    .await?;
                    daemons_started.push("Julia");
                    Some(client)
                } else {
                    None
                };

            let r_client = if cfg.prestart_all_languages || book.uses_language(parser::Language::R)
            {
                let r_cmd = cfg.r.as_deref().unwrap_or("Rscript");
                let client = daemon::DaemonClient::connect_or_spawn(
                    cfg.r_port,
                    r_cmd,
                    parser::Language::R,
                    cfg.idle_timeout,
                )
                .await?;
                daemons_started.push("R");
                Some(client)
            } else {
                None
            };

            if !daemons_started.is_empty() {
                println!(
                    "{} daemon{} ready.",
                    daemons_started.join(" and "),
                    if daemons_started.len() > 1 { "s" } else { "" }
                );
            }

            let clients = runner::Clients {
                julia: jl_client,
                r: r_client,
                fig_width: cfg.fig_width,
                fig_height: cfg.fig_height,
                style: cfg.style.clone(),
            };
            let mut cache = cache::BookCache::load(&cfg.cache_dir, &book)?;

            let summary = match chapter {
                Some(ch) => {
                    if !book.chapters.contains(&ch) {
                        anyhow::bail!(
                            "Chapter '{}' not found. Available: {}",
                            ch,
                            book.chapters.join(", ")
                        );
                    }
                    let mut total = runner::RunSummary::default();
                    if book.chapters.contains(&"preamble".to_string()) && ch != "preamble" {
                        let s = runner::run_chapter(
                            "preamble",
                            &book,
                            &mut cache,
                            &cfg.cache_dir,
                            &clients,
                            force,
                        )
                        .await?;
                        total.cells_executed += s.cells_executed;
                        total.cells_skipped += s.cells_skipped;
                    }
                    let s = runner::run_chapter(
                        &ch, &book, &mut cache, &cfg.cache_dir, &clients, force,
                    )
                    .await?;
                    total.cells_executed += s.cells_executed;
                    total.cells_skipped += s.cells_skipped;
                    codegen::write_cache_typ(&cfg.cache_dir, &book, &cache)?;
                    codegen::write_style_typ(
                        &cfg.style,
                        cfg.data_file.parent().unwrap_or(std::path::Path::new(".")),
                    )?;
                    codegen::write_data_typ(&cfg.data_file, &cfg.cache_dir)?;
                    total
                }
                None => {
                    runner::run_all(
                        &book,
                        &mut cache,
                        &cfg.cache_dir,
                        &clients,
                        force,
                        &cfg.data_file,
                    )
                    .await?
                }
            };

            // Always write style and data files (even if all cells were fresh),
            // so changes to loom.toml [style] take effect without --force.
            codegen::write_style_typ(
                &cfg.style,
                cfg.data_file.parent().unwrap_or(std::path::Path::new(".")),
            )?;
            codegen::write_data_typ(&cfg.data_file, &cfg.cache_dir)?;

            let total = summary.cells_executed + summary.cells_skipped;
            if summary.cells_executed == 0 {
                println!("Done — all {} cells fresh.", total);
            } else {
                println!(
                    "Done — {} cell{} executed, {} cached.",
                    summary.cells_executed,
                    if summary.cells_executed == 1 { "" } else { "s" },
                    summary.cells_skipped,
                );
            }
        }

        Command::Watch {
            root,
            cache_dir,
            port,
            idle_timeout,
            verbose: _,
        } => {
            write_typst_files_if_missing()?;
            let cfg = config::Config::load(port, cache_dir.as_deref(), idle_timeout)?;
            let timeout_desc = if cfg.idle_timeout == 0 {
                "no timeout".to_string()
            } else {
                format!("{}m idle timeout", cfg.idle_timeout / 60)
            };
            println!(
                "Watching {} for changes ({}) — Ctrl-C to stop.",
                root.display(),
                timeout_desc,
            );
            watcher::watch_loop(root, cfg).await?;
        }

        Command::List { root } => {
            let book = parser::parse_book(&root)?;
            println!("{:<20} {:>5}  {}", "CHAPTER", "CELLS", "LANGUAGES");
            println!("{}", "-".repeat(40));
            for ch in &book.chapters {
                let cells = book.chapter_cells(ch);
                let n = cells.len();
                let has_jl = cells.iter().any(|c| c.language == parser::Language::Julia);
                let has_r = cells.iter().any(|c| c.language == parser::Language::R);
                let langs: Vec<&str> = [
                    if has_jl { Some("Julia") } else { None },
                    if has_r { Some("R") } else { None },
                ]
                .into_iter()
                .flatten()
                .collect();
                let tag = if ch == "preamble" { " (preamble)" } else { "" };
                println!("{:<20} {:>5}  {}{}", ch, n, langs.join(", "), tag);
            }
            println!("{}", "-".repeat(40));
            println!(
                "{:<20} {:>5}",
                "TOTAL",
                book.chapters
                    .iter()
                    .map(|c| book.chapter_cells(c).len())
                    .sum::<usize>()
            );
        }

        Command::Daemon { action } => match action {
            DaemonAction::List => {
                let daemons = daemon::list_daemons();
                if daemons.is_empty() {
                    println!("No running loom daemons.");
                } else {
                    println!("{:<10} {}", "PORT", "PID");
                    println!("{}", "-".repeat(20));
                    for (port, pid) in &daemons {
                        println!("{:<10} {}", port, pid);
                    }
                }
            }
            DaemonAction::Stop { port, all } => {
                if all {
                    let daemons = daemon::list_daemons();
                    if daemons.is_empty() {
                        println!("No running loom daemons.");
                    } else {
                        for (p, pid) in &daemons {
                            if daemon::kill_daemon(*p) {
                                println!("Stopped daemon on port {} (PID {}).", p, pid);
                            }
                        }
                    }
                } else {
                    let target_port = match port {
                        Some(p) => p,
                        None => config::Config::load_defaults()?.julia_port,
                    };
                    if daemon::kill_daemon(target_port) {
                        println!("Stopped daemon on port {}.", target_port);
                    } else {
                        println!("No daemon running on port {}.", target_port);
                    }
                }
            }
        },
    }

    Ok(())
}

// ── Embedded Typst rendering files ───────────────────────────────────────────

const LOOM_DIR: &str = ".loom";
const JULIA_TYP: &str = include_str!("../julia.typ.embedded");
const R_TYP: &str = include_str!("../r.typ.embedded");
const LOOM_TYP: &str = include_str!("../loom.typ.embedded");
const DEFAULT_LOOM_TOML: &str = include_str!("../loom.toml");

/// Ensure the `.loom/` directory exists.
fn ensure_loom_dir() -> Result<()> {
    std::fs::create_dir_all(LOOM_DIR)?;
    Ok(())
}

/// Warn if old loom-managed files are still in the project root.
fn warn_stale_root_files() {
    for name in &["julia.typ", "r.typ", "loom.typ"] {
        let path = std::path::Path::new(name);
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                if is_loom_managed_file(&content) {
                    log::warn!(
                        "Stale loom file `{name}` in project root. \
                         Loom now uses .loom/ — you can safely delete `{name}`."
                    );
                }
            }
        }
    }
}

/// Write Typst support files into `.loom/` if they don't exist.
fn write_typst_files_if_missing() -> Result<()> {
    ensure_loom_dir()?;
    sync_managed_file(&format!("{LOOM_DIR}/julia.typ"), JULIA_TYP)?;
    sync_managed_file(&format!("{LOOM_DIR}/r.typ"), R_TYP)?;
    sync_managed_file(&format!("{LOOM_DIR}/loom.typ"), LOOM_TYP)?;
    warn_stale_root_files();
    Ok(())
}

/// Full project initialization: write Typst support files and loom.toml.
fn init_project() -> Result<()> {
    ensure_loom_dir()?;
    sync_managed_file(&format!("{LOOM_DIR}/julia.typ"), JULIA_TYP)?;
    sync_managed_file(&format!("{LOOM_DIR}/r.typ"), R_TYP)?;
    sync_managed_file(&format!("{LOOM_DIR}/loom.typ"), LOOM_TYP)?;
    write_if_missing("loom.toml", DEFAULT_LOOM_TOML)?;

    // Write a placeholder _loom_data.typ so Typst can compile before first run.
    let data_file = format!("{LOOM_DIR}/_loom_data.typ");
    if !std::path::Path::new(&data_file).exists() {
        std::fs::write(
            &data_file,
            "// AUTO-GENERATED by loom — do not edit\n\
             // Run `loom run <file.typ>` to populate.\n\
             #let _loom_style = (:)\n\
             #let _loom = (:)\n",
        )?;
        println!("  Created {data_file}");
    }

    warn_stale_root_files();

    println!("Loom initialized. Files written to .loom/ directory.");
    println!("  .loom/loom.typ   — ergonomic single-entrypoint Loom API");
    println!("  .loom/julia.typ  — Julia rendering functions");
    println!("  .loom/r.typ      — R rendering functions");
    println!("  loom.toml        — configuration (project root)");
    println!();
    println!("Preferred usage in your .typ file:");
    println!("  #import \".loom/loom.typ\": *");
    println!();
    println!("Legacy low-level imports remain supported if needed.");
    Ok(())
}

fn write_if_missing(name: &str, content: &str) -> Result<()> {
    let path = std::path::Path::new(name);
    if !path.exists() {
        std::fs::write(path, content)?;
        println!("  Created {name}");
    } else {
        log::debug!("{name} already exists, skipping.");
    }
    Ok(())
}

fn sync_managed_file(name: &str, content: &str) -> Result<()> {
    let path = std::path::Path::new(name);
    if !path.exists() {
        std::fs::write(path, content)?;
        println!("  Created {name}");
        return Ok(());
    }

    let existing = std::fs::read_to_string(path)?;
    if existing == content {
        log::debug!("{name} already up to date.");
        return Ok(());
    }

    if is_loom_managed_file(&existing) {
        std::fs::write(path, content)?;
        println!("  Updated {name}");
    } else {
        log::warn!(
            "{name} exists and does not look loom-managed; leaving it unchanged."
        );
    }

    Ok(())
}

fn is_loom_managed_file(content: &str) -> bool {
    content.starts_with("// julia.typ — Julia code rendering functions for loom")
        || content.starts_with("// r.typ — R code rendering functions for loom")
        || content.starts_with("// loom.typ — ergonomic single-entrypoint API for Loom")
}
