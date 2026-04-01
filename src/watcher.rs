use crate::config::Config;
use crate::daemon::DaemonClient;
/// File watcher — incremental re-execution for Tinymist live preview.
use crate::{cache, parser, runner};
use anyhow::Result;
use notify::{self, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use tokio::sync::mpsc;

pub async fn watch_loop(root: PathBuf, config: Config) -> Result<()> {
    log::info!("Watching {} for changes — Ctrl-C to stop.", root.display());

    let initial_book = parser::parse_book(&root)?;

    let jl_client =
        if config.prestart_all_languages || initial_book.uses_language(parser::Language::Julia) {
            Some(
                DaemonClient::connect_or_spawn(
                    config.julia_port,
                    &config.julia,
                    parser::Language::Julia,
                    config.idle_timeout,
                )
                .await?,
            )
        } else {
            None
        };

    let r_client =
        if config.prestart_all_languages || initial_book.uses_language(parser::Language::R) {
            let r_cmd = config.r.as_deref().unwrap_or("Rscript");
            Some(
                DaemonClient::connect_or_spawn(
                    config.r_port,
                    r_cmd,
                    parser::Language::R,
                    config.idle_timeout,
                )
                .await?,
            )
        } else {
            None
        };

    let mut clients = runner::Clients {
        julia: jl_client,
        r: r_client,
        fig_width: config.fig_width,
        fig_height: config.fig_height,
        style: config.style.clone(),
    };
    let cache_dir = config.cache_dir.clone();
    let data_file = config.data_file.clone();

    let (tx, mut rx) = mpsc::channel::<PathBuf>(64);

    let watch_root = root.parent().unwrap_or(&root).to_path_buf();
    let data_file_name = data_file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let _watcher = {
        let tx = tx.clone();
        let data_file_name = data_file_name.clone();
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                if let Ok(event) = res {
                    for path in event.paths {
                        if path.extension().and_then(|e| e.to_str()) == Some("typ") {
                            let lossy = path.to_string_lossy();
                            if !lossy.contains(".loom") && !lossy.ends_with(&data_file_name) {
                                let _ = tx.blocking_send(path);
                            }
                        }
                    }
                }
            },
            notify::Config::default(),
        )?;
        watcher.watch(&watch_root, RecursiveMode::Recursive)?;
        watcher
    };

    // Initial full run.
    {
        log::info!("Initial run…");
        let book = initial_book;
        let mut cache = cache::BookCache::load(&cache_dir, &book)?;
        if let Err(e) =
            runner::run_all(&book, &mut cache, &cache_dir, &clients, false, &data_file).await
        {
            log::error!("Initial run failed: {e}");
        }
    }

    // Event loop with 200 ms debounce.
    loop {
        let first = match rx.recv().await {
            Some(p) => p,
            None => break,
        };

        let mut changed: std::collections::HashSet<PathBuf> = Default::default();
        changed.insert(first);

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(200);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(p)) => {
                    changed.insert(p);
                }
                _ => break,
            }
        }

        log::info!(
            "{} file(s) changed: {}",
            changed.len(),
            changed
                .iter()
                .map(|p| p.file_name().unwrap_or_default().to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ")
        );

        let book = match parser::parse_book(&root) {
            Ok(b) => b,
            Err(e) => {
                log::error!("Parse error: {e}");
                continue;
            }
        };

        if book.uses_language(parser::Language::Julia) && clients.julia.is_none() {
            match DaemonClient::connect_or_spawn(
                config.julia_port,
                &config.julia,
                parser::Language::Julia,
                config.idle_timeout,
            )
            .await
            {
                Ok(client) => clients.julia = Some(client),
                Err(e) => {
                    log::error!("Failed to start Julia daemon: {e}");
                    continue;
                }
            }
        }

        if !config.prestart_all_languages && !book.uses_language(parser::Language::Julia) {
            clients.julia = None;
        }

        if book.uses_language(parser::Language::R) && clients.r.is_none() {
            let r_cmd = config.r.as_deref().unwrap_or("Rscript");
            match DaemonClient::connect_or_spawn(
                config.r_port,
                r_cmd,
                parser::Language::R,
                config.idle_timeout,
            )
            .await
            {
                Ok(client) => clients.r = Some(client),
                Err(e) => {
                    log::error!("Failed to start R daemon: {e}");
                    continue;
                }
            }
        }

        if !config.prestart_all_languages && !book.uses_language(parser::Language::R) {
            clients.r = None;
        }

        let mut cache = match cache::BookCache::load(&cache_dir, &book) {
            Ok(c) => c,
            Err(e) => {
                log::error!("Cache load error: {e}");
                continue;
            }
        };

        for path in &changed {
            match runner::run_affected(path, &book, &mut cache, &cache_dir, &clients, &data_file)
                .await
            {
                Ok(true) => {}
                Ok(false) => log::debug!("No cells in changed file: {}", path.display()),
                Err(e) => log::error!("Run error: {e}"),
            }
        }
    }

    Ok(())
}
