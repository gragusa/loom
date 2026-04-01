/// Language daemon client — speaks JSON-over-TCP to Julia and R daemons.
///
/// Both daemon scripts are embedded in the binary at compile time and
/// written to temp files on first use.
use crate::parser::{Cell, CellKind, Language};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;

/// Embedded daemon scripts.
const JULIA_DAEMON_SCRIPT: &str = include_str!("../julia-daemon.jl");
const R_DAEMON_SCRIPT: &str = include_str!("../r-daemon.R");

const CONNECT_RETRIES: u32 = 20;
const RETRY_BASE_MS: u64 = 250;

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Request {
    session: String,
    id: String,
    code: String,
    op: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    preamble_code: String,
    /// knitr-style options (R cells). Julia daemon ignores unknown fields.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    options: HashMap<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Statement {
    pub code: String,
    pub output: String,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct CellResult {
    pub session: String,
    pub id: String,
    pub stdout: String,
    pub stderr: String,
    pub figures: Vec<PathBuf>,
    pub error: Option<String>,
    #[serde(default)]
    pub statements: Vec<Statement>,
    #[serde(default)]
    pub typst_output: String,
}

// ── Client ────────────────────────────────────────────────────────────────────

/// Protocol used by the daemon.
#[derive(Debug, Clone, Copy)]
pub enum Protocol {
    /// Newline-delimited JSON over raw TCP (Julia daemon).
    Tcp,
    /// JSON over HTTP POST (R daemon via httpuv).
    Http,
}

pub struct DaemonClient {
    port: u16,
    protocol: Protocol,
    owns_process: bool,
    #[allow(dead_code)]
    idle_timeout: u64,
}

impl DaemonClient {
    pub async fn connect_or_spawn(
        port: u16,
        cmd: &str,
        language: Language,
        idle_timeout: u64,
    ) -> Result<Self> {
        let protocol = match language {
            Language::Julia => Protocol::Tcp,
            Language::R => Protocol::Http,
        };

        // Always write the latest daemon script.
        write_daemon_script(language)?;

        // Check if a daemon is already running on this port.
        let alive = match protocol {
            Protocol::Tcp => try_connect(port).await.is_ok(),
            Protocol::Http => try_http_ping(port).await.is_ok(),
        };

        let mut owns_process = false;
        if alive {
            // Verify the daemon's working directory matches ours.
            // If not, kill it and respawn so figures save to the right place.
            let needs_restart = match protocol {
                Protocol::Http => match check_daemon_cwd(port).await {
                    Ok(daemon_cwd) => {
                        let our_cwd = std::env::current_dir()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        daemon_cwd != our_cwd
                    }
                    Err(_) => true,
                },
                Protocol::Tcp => false, // Julia daemon: trust it for now
            };

            if needs_restart {
                log::info!(
                    "{:?} daemon on port {port} has wrong working directory, restarting…",
                    language
                );
                kill_daemon(port);
                // Also try to kill by port in case PID file is missing.
                kill_process_on_port(port);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                spawn_daemon(port, cmd, language, idle_timeout).await?;
                owns_process = true;
            }
        } else {
            log::info!("{:?} daemon not found on port {port}, spawning…", language);
            spawn_daemon(port, cmd, language, idle_timeout).await?;
            owns_process = true;
        }

        Ok(Self {
            port,
            protocol,
            owns_process,
            idle_timeout,
        })
    }

    /// Execute a cell in its named session.
    /// `default_fig_width`/`default_fig_height` come from `loom.toml` and
    /// are used when the chunk doesn't specify `fig-width`/`fig-height`.
    pub async fn run_cell(
        &self,
        cell: &Cell,
        default_fig_width: f64,
        default_fig_height: f64,
    ) -> Result<CellResult> {
        let op = match cell.kind {
            CellKind::Console => "console",
            CellKind::Plot => "plot",
            CellKind::Table => "table",
            CellKind::Expr => "expr",
            CellKind::Code => "console",
            CellKind::Inline => "expr",
            CellKind::Run => "run",
        };

        // Convert cell options to JSON values.
        let mut options: HashMap<String, serde_json::Value> = HashMap::new();
        for (k, v) in &cell.options {
            let val = if v == "true" {
                serde_json::Value::Bool(true)
            } else if v == "false" {
                serde_json::Value::Bool(false)
            } else if let Ok(n) = v.parse::<f64>() {
                serde_json::json!(n)
            } else {
                serde_json::Value::String(v.clone())
            };
            options.insert(k.clone(), val);
        }

        // Inject config defaults for fig dimensions if not specified per-chunk.
        if !options.contains_key("fig-width") && !options.contains_key("fig_width") {
            options.insert("fig_width".into(), serde_json::json!(default_fig_width));
        }
        if !options.contains_key("fig-height") && !options.contains_key("fig_height") {
            options.insert("fig_height".into(), serde_json::json!(default_fig_height));
        }

        self.send(Request {
            session: cell.session.clone(),
            id: cell.id.clone(),
            code: cell.code.clone(),
            op: op.into(),
            preamble_code: String::new(),
            options,
        })
        .await
    }

    /// Reset a named session, optionally replaying preamble code first.
    pub async fn reset_session(&self, session: &str, preamble_code: &str) -> Result<()> {
        let result = self
            .send(Request {
                session: session.into(),
                id: "__reset__".into(),
                code: String::new(),
                op: "reset".into(),
                preamble_code: preamble_code.to_string(),
                options: HashMap::new(),
            })
            .await?;
        if let Some(err) = result.error {
            anyhow::bail!("error replaying session seed into '{}': {}", session, err);
        }
        Ok(())
    }

    /// Check daemon is alive.
    #[allow(dead_code)]
    pub async fn ping(&self) -> Result<()> {
        self.send(Request {
            session: "__ping__".into(),
            id: "__ping__".into(),
            code: String::new(),
            op: "ping".into(),
            preamble_code: String::new(),
            options: HashMap::new(),
        })
        .await?;
        Ok(())
    }

    async fn send(&self, req: Request) -> Result<CellResult> {
        match self.protocol {
            Protocol::Tcp => self.send_tcp(req).await,
            Protocol::Http => self.send_http(req).await,
        }
    }

    async fn send_tcp(&self, req: Request) -> Result<CellResult> {
        let mut stream = connect_with_retry(self.port).await?;
        let (reader, mut writer) = stream.split();
        let mut reader = BufReader::new(reader);

        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await?;

        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await?;
        serde_json::from_str(resp_line.trim()).context("Failed to parse daemon response")
    }

    async fn send_http(&self, req: Request) -> Result<CellResult> {
        let body = serde_json::to_vec(&req)?;
        let url = format!("http://127.0.0.1:{}/eval", self.port);

        // Build a minimal HTTP POST request manually (avoids adding reqwest dep).
        let mut stream = TcpStream::connect(("127.0.0.1", self.port))
            .await
            .context("Failed to connect to R daemon")?;

        let http_req = format!(
            "POST /eval HTTP/1.1\r\n\
             Host: 127.0.0.1:{}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n",
            self.port,
            body.len()
        );

        stream.write_all(http_req.as_bytes()).await?;
        stream.write_all(&body).await?;
        stream.flush().await?;

        // Read the full HTTP response.
        let mut response = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            response.extend_from_slice(&buf[..n]);
        }

        let response_str = String::from_utf8_lossy(&response);
        // Extract JSON body after the HTTP headers (separated by \r\n\r\n).
        let json_body = response_str.split("\r\n\r\n").nth(1).unwrap_or("").trim();

        serde_json::from_str(json_body)
            .with_context(|| format!("Failed to parse R daemon response from {url}"))
    }
}

impl Drop for DaemonClient {
    fn drop(&mut self) {
        if self.owns_process {
            let _ = kill_daemon(self.port);
        }
    }
}

// ── Daemon process management ─────────────────────────────────────────────────

fn write_daemon_script(language: Language) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("loom");
    std::fs::create_dir_all(&dir)?;
    let (name, content) = match language {
        Language::Julia => ("julia-daemon.jl", JULIA_DAEMON_SCRIPT),
        Language::R => ("r-daemon.R", R_DAEMON_SCRIPT),
    };
    let path = dir.join(name);
    std::fs::write(&path, content)?;
    Ok(path)
}

pub async fn spawn_daemon(
    port: u16,
    cmd: &str,
    language: Language,
    idle_timeout: u64,
) -> Result<()> {
    // Kill any stale daemon on this port before spawning a new one.
    kill_daemon(port);

    let script = write_daemon_script(language)?;
    log::info!(
        "Spawning {:?} daemon: {} {} --port {} --idle-timeout {}",
        language,
        cmd,
        script.display(),
        port,
        idle_timeout,
    );

    // Redirect daemon stdout to null; stderr to a log file for debugging.
    let null = std::process::Stdio::null();
    let log_dir = std::env::temp_dir().join("loom");
    std::fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join(match language {
        Language::Julia => "julia-daemon.log",
        Language::R => "r-daemon.log",
    });
    log::info!("Daemon log file: {}", log_path.display());
    let log_file = std::fs::File::create(&log_path)
        .with_context(|| format!("Cannot create daemon log file {}", log_path.display()))?;

    match language {
        Language::Julia => {
            ensure_julia_environment(cmd).await?;
            Command::new(cmd)
                .arg("--startup-file=no")
                .arg("--project=.")
                .arg(&script)
                .arg("--port")
                .arg(port.to_string())
                .arg("--idle-timeout")
                .arg(idle_timeout.to_string())
                .stdout(null)
                .stderr(std::process::Stdio::from(log_file))
                .spawn()
                .with_context(|| {
                    format!("Failed to spawn Julia daemon — is `{cmd}` on your PATH?")
                })?;
        }
        Language::R => {
            ensure_r_environment(cmd).await?;
            Command::new(cmd)
                .arg("--no-save")
                .arg("--no-restore")
                .arg(script.to_str().unwrap())
                .arg("--port")
                .arg(port.to_string())
                .arg("--idle-timeout")
                .arg(idle_timeout.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::from(log_file))
                .spawn()
                .with_context(|| format!("Failed to spawn R daemon — is `{cmd}` on your PATH?"))?;
        }
    }

    log::info!(
        "Waiting for {:?} daemon (first run may take a moment)…",
        language
    );
    match language {
        Language::Julia => {
            connect_with_retry(port).await?;
        }
        Language::R => {
            http_wait_ready(port).await?;
        }
    }
    log::info!("{:?} daemon ready on port {port}.", language);
    Ok(())
}

async fn ensure_julia_environment(cmd: &str) -> Result<()> {
    if julia_package_available(cmd, "JSON3").await? {
        return Ok(());
    }

    let has_project = Path::new("Project.toml").exists();
    let prompt = if has_project {
        "Julia environment found in the current directory, but Loom needs JSON3. Add it now? [Y/n]: "
    } else {
        "No Julia environment found in the current directory. Create one and add Loom's dependency JSON3 now? [Y/n]: "
    };

    if !prompt_yes_no(prompt)? {
        anyhow::bail!(
            "Julia environment is missing Loom dependency `JSON3`. \
             Run `julia --project=. -e 'using Pkg; Pkg.add(\"JSON3\")'`{} and retry.",
            if has_project {
                ""
            } else {
                " after creating/activating a local project"
            }
        );
    }

    install_julia_package(cmd, "JSON3", has_project).await?;

    if !julia_package_available(cmd, "JSON3").await? {
        anyhow::bail!(
            "Julia dependency installation completed, but `JSON3` is still not available in the current project."
        );
    }

    Ok(())
}

async fn julia_package_available(cmd: &str, package: &str) -> Result<bool> {
    let status = Command::new(cmd)
        .arg("--startup-file=no")
        .arg("--project=.")
        .arg("-e")
        .arg(format!("using {package}"))
        .status()
        .await
        .with_context(|| format!("Failed to probe Julia package `{package}`"))?;

    Ok(status.success())
}

async fn install_julia_package(cmd: &str, package: &str, has_project: bool) -> Result<()> {
    let script = if has_project {
        format!("using Pkg; Pkg.add(\"{package}\")")
    } else {
        format!("using Pkg; Pkg.activate(\".\"); Pkg.add(\"{package}\")")
    };

    let status = Command::new(cmd)
        .arg("--startup-file=no")
        .arg("--project=.")
        .arg("-e")
        .arg(script)
        .status()
        .await
        .with_context(|| format!("Failed to install Julia package `{package}`"))?;

    if !status.success() {
        anyhow::bail!("Julia package installation failed for `{package}`");
    }

    Ok(())
}

async fn ensure_r_environment(cmd: &str) -> Result<()> {
    let required: &[&str] = &["jsonlite", "httpuv"];

    let mut missing = Vec::new();
    for &pkg in required {
        if !r_package_available(cmd, pkg).await? {
            missing.push(pkg);
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    let use_renv = Path::new("renv.lock").exists();
    let missing_list = missing.join(", ");

    if use_renv {
        if !r_package_available(cmd, "renv").await? {
            anyhow::bail!(
                "An renv.lock file was found but the `renv` package is not installed. \
                 Run `Rscript -e 'install.packages(\"renv\"); renv::restore()'` first."
            );
        }
    }

    let prompt = if use_renv {
        format!(
            "Loom needs R packages [{missing_list}] but they are not available. \
             An renv environment was detected. Install via renv::install()? [Y/n]: "
        )
    } else {
        format!(
            "Loom needs R packages [{missing_list}] but they are not installed. \
             Install via install.packages()? [Y/n]: "
        )
    };

    if !prompt_yes_no(&prompt)? {
        anyhow::bail!(
            "R environment is missing Loom dependencies: {missing_list}. \
             Install them manually and retry."
        );
    }

    install_r_packages(cmd, &missing, use_renv).await?;

    let mut still_missing = Vec::new();
    for &pkg in &missing {
        if !r_package_available(cmd, pkg).await? {
            still_missing.push(pkg);
        }
    }

    if !still_missing.is_empty() {
        anyhow::bail!(
            "R package installation completed, but these packages are still \
             not available: {}",
            still_missing.join(", ")
        );
    }

    Ok(())
}

async fn r_package_available(cmd: &str, package: &str) -> Result<bool> {
    let status = Command::new(cmd)
        .arg("--no-save")
        .arg("--no-restore")
        .arg("-e")
        .arg(format!(
            "if (!requireNamespace(\"{package}\", quietly = TRUE)) quit(status = 1)"
        ))
        .status()
        .await
        .with_context(|| format!("Failed to probe R package `{package}`"))?;

    Ok(status.success())
}

async fn install_r_packages(cmd: &str, packages: &[&str], use_renv: bool) -> Result<()> {
    let pkg_vec = packages
        .iter()
        .map(|p| format!("\"{}\"", p))
        .collect::<Vec<_>>()
        .join(", ");

    let script = if use_renv {
        format!("renv::install(c({pkg_vec}))")
    } else {
        format!(
            "install.packages(c({pkg_vec}), repos = \"https://cloud.r-project.org\")"
        )
    };

    let status = Command::new(cmd)
        .arg("--no-save")
        .arg("--no-restore")
        .arg("-e")
        .arg(&script)
        .status()
        .await
        .with_context(|| {
            format!("Failed to install R packages: {}", packages.join(", "))
        })?;

    if !status.success() {
        anyhow::bail!(
            "R package installation failed for: {}",
            packages.join(", ")
        );
    }

    Ok(())
}

fn prompt_yes_no(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_ascii_lowercase();

    Ok(answer.is_empty() || answer == "y" || answer == "yes")
}

/// PID file path for a given port.
pub fn pid_file_path(port: u16) -> PathBuf {
    std::env::temp_dir().join(format!("loom-daemon-{port}.pid"))
}

/// List all running loom daemons by scanning PID files in tempdir.
pub fn list_daemons() -> Vec<(u16, u32)> {
    let tmp = std::env::temp_dir();
    let mut daemons = Vec::new();
    let mut seen_ports = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(&tmp) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(rest) = name.strip_prefix("loom-daemon-") {
                if let Some(port_str) = rest.strip_suffix(".pid") {
                    if let Ok(port) = port_str.parse::<u16>() {
                        if let Ok(pid_str) = std::fs::read_to_string(entry.path()) {
                            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                                if is_pid_alive(pid) {
                                    daemons.push((port, pid));
                                    seen_ports.insert(port);
                                } else {
                                    let _ = std::fs::remove_file(entry.path());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    for port in candidate_ports() {
        if seen_ports.contains(&port) {
            continue;
        }

        if daemon_alive_on_port(port) {
            if let Some(pid) = pid_for_port(port) {
                daemons.push((port, pid));
                seen_ports.insert(port);
            }
        }
    }

    daemons.sort();
    daemons
}

/// Kill a daemon by port. Returns true if a process was killed.
pub fn kill_daemon(port: u16) -> bool {
    let path = pid_file_path(port);
    let mut killed = false;
    if let Ok(pid_str) = std::fs::read_to_string(&path) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            unsafe {
                if libc::kill(pid, libc::SIGTERM) == 0 {
                    killed = true;
                }
            }
        }
    }
    let _ = std::fs::remove_file(&path);
    // Also try to kill by port in case PID file was missing/stale.
    if !killed {
        kill_process_on_port(port);
        killed = true; // assume success; harmless if nothing was there
    }
    killed
}

fn is_pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn candidate_ports() -> Vec<u16> {
    let mut ports = Vec::new();
    let defaults = crate::config::Config::load_defaults().ok();

    if let Some(cfg) = defaults {
        ports.push(cfg.julia_port);
        ports.push(cfg.r_port);
    } else {
        ports.push(2159);
        ports.push(2160);
    }

    ports.sort_unstable();
    ports.dedup();
    ports
}

fn daemon_alive_on_port(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

fn pid_for_port(port: u16) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("lsof")
            .args(["-ti", &format!(":{port}")])
            .output()
        {
            let out = String::from_utf8_lossy(&output.stdout);
            for line in out.lines() {
                if let Ok(pid) = line.trim().parse::<u32>() {
                    return Some(pid);
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = std::process::Command::new("ss")
            .args(["-tlnp", &format!("sport = :{port}")])
            .output()
        {
            let out = String::from_utf8_lossy(&output.stdout);
            if let Some(pid_start) = out.find("pid=") {
                let rest = &out[pid_start + 4..];
                if let Some(end) = rest.find(|c: char| !c.is_ascii_digit()) {
                    if let Ok(pid) = rest[..end].parse::<u32>() {
                        return Some(pid);
                    }
                }
            }
        }
    }

    None
}

// ── Connection helpers ────────────────────────────────────────────────────────

/// Ask the R daemon for its working directory.
async fn check_daemon_cwd(port: u16) -> Result<String> {
    let body = r#"{"op":"run","session":"__cwd__","id":"__cwd__","code":"cat(getwd())"}"#;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let req = format!(
        "POST /eval HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        port,
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    let mut response = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buf[..n]);
    }
    let resp_str = String::from_utf8_lossy(&response);
    let json_body = resp_str.split("\r\n\r\n").nth(1).unwrap_or("").trim();
    let v: serde_json::Value = serde_json::from_str(json_body)?;
    Ok(v["stdout"].as_str().unwrap_or("").to_string())
}

/// Kill any process listening on the given port (fallback when PID file is missing).
fn kill_process_on_port(port: u16) {
    // Try to read /proc/net/tcp or use ss/lsof — but simplest: just try kill_daemon again.
    // On Linux, we can parse /proc/net/tcp6 or /proc/net/tcp.
    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = std::process::Command::new("ss")
            .args(["-tlnp", &format!("sport = :{port}")])
            .output()
        {
            let out = String::from_utf8_lossy(&output.stdout);
            // Extract PID from output like: users:(("R",pid=12345,fd=15))
            if let Some(pid_start) = out.find("pid=") {
                let rest = &out[pid_start + 4..];
                if let Some(end) = rest.find(|c: char| !c.is_ascii_digit()) {
                    if let Ok(pid) = rest[..end].parse::<i32>() {
                        unsafe {
                            libc::kill(pid, libc::SIGTERM);
                        }
                        log::debug!("Killed process {pid} on port {port}");
                    }
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("lsof")
            .args(["-ti", &format!(":{port}")])
            .output()
        {
            let out = String::from_utf8_lossy(&output.stdout);
            for line in out.lines() {
                if let Ok(pid) = line.trim().parse::<i32>() {
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                    log::debug!("Killed process {pid} on port {port}");
                }
            }
        }
    }
}

async fn try_connect(port: u16) -> Result<TcpStream> {
    Ok(TcpStream::connect(("127.0.0.1", port)).await?)
}

async fn try_http_ping(port: u16) -> Result<()> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let body = r#"{"op":"ping","session":"__ping__","id":"__ping__","code":""}"#;
    let req = format!(
        "POST /eval HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        port,
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    let mut buf = [0u8; 256];
    let n = stream.read(&mut buf).await?;
    if n > 0 && String::from_utf8_lossy(&buf[..n]).contains("200") {
        Ok(())
    } else {
        anyhow::bail!("HTTP ping failed")
    }
}

async fn http_wait_ready(port: u16) -> Result<()> {
    let mut delay = RETRY_BASE_MS;
    for attempt in 1..=CONNECT_RETRIES {
        match try_http_ping(port).await {
            Ok(()) => return Ok(()),
            Err(_) => {
                if attempt == CONNECT_RETRIES {
                    anyhow::bail!(
                        "Could not connect to R daemon on port {port} after {CONNECT_RETRIES} attempts"
                    );
                }
                log::debug!("R daemon attempt {attempt} failed, retrying in {delay}ms…");
                tokio::time::sleep(Duration::from_millis(delay)).await;
                delay = (delay * 2).min(4000);
            }
        }
    }
    unreachable!()
}

async fn connect_with_retry(port: u16) -> Result<TcpStream> {
    let mut delay = RETRY_BASE_MS;
    for attempt in 1..=CONNECT_RETRIES {
        match try_connect(port).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                if attempt == CONNECT_RETRIES {
                    return Err(e).context(format!(
                        "Could not connect to daemon on port {port} \
                         after {CONNECT_RETRIES} attempts"
                    ));
                }
                log::debug!("Attempt {attempt} failed, retrying in {delay}ms…");
                tokio::time::sleep(Duration::from_millis(delay)).await;
                delay = (delay * 2).min(4000);
            }
        }
    }
    unreachable!()
}
