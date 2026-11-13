//! Kernel processes and the client side of `kernels/PROTOCOL.md`.
//!
//! Each `loom` process starts its own kernels and is their only client. A
//! kernel exits when its parent `loom` process disappears, so kernels never
//! outlive the command that started them.

use crate::config::{LangConfig, Project};
use crate::syntax::Lang;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: u64 = 1;

/// Process ids of the running kernels, so that a signal handler can stop
/// them before loom exits.
static KERNEL_PIDS: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

/// The project lock held by this process, removed by `kill_all_and_exit`.
static LOCK_PATH: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Kill every kernel, release the project lock, and exit. Kernels cannot
/// notice their parent's exit while a cell computes, so loom stops them.
pub fn kill_all_and_exit(code: i32) -> ! {
    if let Ok(pids) = KERNEL_PIDS.lock() {
        for &pid in pids.iter() {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
    if let Ok(Some(path)) = LOCK_PATH.lock().as_deref() {
        let _ = std::fs::remove_file(path);
    }
    std::process::exit(code)
}
const START_TIMEOUT: Duration = Duration::from_secs(180);

const JULIA_SCRIPT: &str = include_str!("../kernels/julia.jl");
const R_SCRIPT: &str = include_str!("../kernels/r.R");
const PYTHON_SCRIPT: &str = include_str!("../kernels/python.py");

fn script(lang: Lang) -> (&'static str, &'static str) {
    match lang {
        Lang::Julia => ("julia.jl", JULIA_SCRIPT),
        Lang::R => ("r.R", R_SCRIPT),
        Lang::Python => ("python.py", PYTHON_SCRIPT),
    }
}

/// An `exec` request.
pub struct ExecRequest<'a> {
    pub session: &'a str,
    pub after: &'a str,
    pub id: &'a str,
    pub inline: bool,
    pub code: &'a str,
    pub options: Value,
    pub file: &'a str,
    pub line: usize,
    /// Name and absolute path of each imported value.
    pub imports: Vec<(String, String)>,
    pub exports: &'a [String],
}

/// Result of a `reset` or `exec` request.
#[derive(Debug)]
pub struct Reply {
    /// User code raised an error.
    pub failed: bool,
    pub outputs: Vec<Value>,
    /// Names the request created, rebound, or removed (`exec` only).
    pub changed: Vec<String>,
    /// Packages loaded in the kernel process (`exec` only).
    pub packages: Vec<Value>,
    /// Exported values: name and file in the scratch directory.
    pub exports: Vec<(String, String)>,
}

pub struct Kernel {
    pub lang: Lang,
    pub version: String,
    child: Child,
    /// Process id the kernel reports for itself. It differs from the child
    /// process when the command is a launcher (juliaup, for example) that
    /// starts the language in a process of its own; signals must go here.
    kernel_pid: u32,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    token: String,
    /// Position of each session, as last confirmed by the kernel.
    positions: HashMap<String, String>,
    log_path: PathBuf,
    registry_path: PathBuf,
}

impl Kernel {
    pub fn start(lang: Lang, cfg: &LangConfig, project: &Project) -> Result<Self> {
        let dir = project.kernels_dir();
        std::fs::create_dir_all(&dir)?;
        let (script_name, script_text) = script(lang);
        let script_path = dir.join(script_name);
        std::fs::write(&script_path, script_text)
            .with_context(|| format!("cannot write {}", script_path.display()))?;
        let scratch = dir.join(format!("scratch-{}", lang.tag()));
        std::fs::create_dir_all(&scratch)?;
        let log_path = dir.join(format!("{}.log", lang.tag()));
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("cannot open {}", log_path.display()))?;
        writeln!(
            log,
            "--- loom {} starting {} kernel at unix time {}",
            env!("CARGO_PKG_VERSION"),
            lang.name(),
            crate::store::now()
        )?;

        // The kernel connects to loom rather than listening itself: base R
        // cannot restrict a listening socket to the loopback interface.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .context("cannot listen on 127.0.0.1")?;
        let port = listener.local_addr()?.port();
        let token = random_token();
        let command = cfg.command_for(lang, &project.dir);
        let mut cmd = Command::new(&command);
        cmd.args(cfg.args_for(lang))
            .arg(&script_path)
            .args(["--connect", &port.to_string()])
            .args(["--token", &token])
            .args(["--parent-pid", &std::process::id().to_string()])
            .arg("--scratch")
            .arg(&scratch)
            .current_dir(&project.dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log.try_clone()?))
            // Own process group: Ctrl-C in the terminal reaches loom, which
            // then stops its kernels, instead of killing them mid-request.
            .process_group(0);
        log::info!("starting {} kernel: {:?}", lang.name(), cmd);
        let child = cmd.spawn().with_context(|| {
            format!(
                "cannot start the {} kernel with `{}`; set [{}] command in loom.toml",
                lang.name(),
                command,
                lang.tag()
            )
        })?;

        let registry_path = dir.join(format!("{}.json", lang.tag()));
        let mut kernel = Self::connect(lang, child, listener, token, log_path, registry_path)?;
        KERNEL_PIDS
            .lock()
            .expect("lock")
            .extend([kernel.child.id(), kernel.kernel_pid]);
        std::fs::write(
            &kernel.registry_path,
            serde_json::to_vec(&json!({
                "pid": kernel.kernel_pid,
                "loom_pid": std::process::id(),
                "version": kernel.version,
            }))?,
        )?;
        kernel.positions.clear();
        Ok(kernel)
    }

    fn connect(
        lang: Lang,
        mut child: Child,
        listener: TcpListener,
        token: String,
        log_path: PathBuf,
        registry_path: PathBuf,
    ) -> Result<Self> {
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + START_TIMEOUT;
        let stream = loop {
            if let Some(status) = child.try_wait()? {
                bail!(
                    "the {} kernel exited during startup ({status})\n{}",
                    lang.name(),
                    log_tail(&log_path)
                );
            }
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(e) => {
                    let _ = child.kill();
                    bail!(
                        "the {} kernel did not connect within {} s: {e}\n{}",
                        lang.name(),
                        START_TIMEOUT.as_secs(),
                        log_tail(&log_path)
                    );
                }
            }
        };
        drop(listener);
        stream.set_nonblocking(false)?;
        // The first line identifies the kernel; any other local process that
        // connected first is turned away here. A kernel sends nothing else
        // before the first request, so this short-lived reader cannot buffer
        // data that belongs to a later response.
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut first = String::new();
        BufReader::new(&stream).read_line(&mut first)?;
        let presented = serde_json::from_str::<Value>(&first)
            .ok()
            .and_then(|v| v.get("token").and_then(Value::as_str).map(str::to_string));
        if presented.as_deref() != Some(token.as_str()) {
            let _ = child.kill();
            bail!(
                "the process that connected as the {} kernel did not present the expected token",
                lang.name()
            );
        }
        stream.set_read_timeout(None)?;
        stream.set_nodelay(true)?;
        let writer = stream.try_clone()?;
        let mut kernel = Self {
            lang,
            version: String::new(),
            kernel_pid: 0,
            child,
            reader: BufReader::new(stream),
            writer,
            token,
            positions: HashMap::new(),
            log_path,
            registry_path,
        };
        let hello = kernel.request(json!({ "op": "hello" }))?;
        let protocol = hello.get("protocol").and_then(Value::as_u64);
        let language = hello.get("language").and_then(Value::as_str);
        if protocol != Some(PROTOCOL_VERSION) || language != Some(lang.tag()) {
            bail!(
                "unexpected hello from the {} kernel: {hello}",
                lang.name()
            );
        }
        kernel.version = hello
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        kernel.kernel_pid = hello
            .get("pid")
            .and_then(Value::as_u64)
            .context("the kernel's hello lacks `pid`")? as u32;
        Ok(kernel)
    }

    /// Process to signal to interrupt a running cell.
    pub fn pid(&self) -> u32 {
        self.kernel_pid
    }

    /// The session's position, if the kernel has the session.
    pub fn position(&self, session: &str) -> Option<&str> {
        self.positions.get(session).map(String::as_str)
    }

    pub fn reset(&mut self, session: &str, position: &str, setup: &[String]) -> Result<Reply> {
        self.positions.remove(session);
        let resp = self.request(json!({
            "op": "reset",
            "session": session,
            "position": position,
            "setup": setup,
        }))?;
        let reply = parse_reply(resp, false)?;
        if !reply.failed {
            self.positions.insert(session.to_string(), position.to_string());
        }
        Ok(reply)
    }

    pub fn exec(&mut self, req: ExecRequest) -> Result<Reply> {
        let mut msg = json!({
            "op": "exec",
            "session": req.session,
            "after": req.after,
            "id": req.id,
            "mode": if req.inline { "inline" } else { "block" },
            "code": req.code,
            "options": req.options,
            "file": req.file,
            "line": req.line,
        });
        if !req.imports.is_empty() {
            msg["imports"] = req
                .imports
                .iter()
                .map(|(name, file)| json!({ "name": name, "file": file }))
                .collect();
        }
        if !req.exports.is_empty() {
            msg["exports"] = json!(req.exports);
        }
        let session = req.session;
        let id = req.id;
        let resp = self.request(msg)?;
        let reply = parse_reply(resp, true)?;
        self.positions.insert(session.to_string(), id.to_string());
        Ok(reply)
    }

    /// Mark the session's state as unknown, so it is reset before its next use.
    pub fn forget(&mut self, session: &str) {
        self.positions.remove(session);
    }

    pub fn shutdown(mut self) {
        if self.request(json!({ "op": "shutdown" })).is_ok() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = self.child.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        // Drop kills the process if it is still running.
    }

    fn request(&mut self, mut req: Value) -> Result<Value> {
        req["token"] = Value::String(self.token.clone());
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        let op = req["op"].as_str().unwrap_or("?").to_string();
        self.writer
            .write_all(line.as_bytes())
            .and_then(|_| self.writer.flush())
            .map_err(|e| self.died(&op, e.into()))?;
        let mut buf = String::new();
        let n = self
            .reader
            .read_line(&mut buf)
            .map_err(|e| self.died(&op, e.into()))?;
        if n == 0 {
            return Err(self.died(&op, anyhow::anyhow!("connection closed")));
        }
        let resp: Value = serde_json::from_str(&buf).with_context(|| {
            format!("invalid response from the {} kernel: {buf}", self.lang.name())
        })?;
        if resp.get("ok") != Some(&Value::Bool(true)) {
            let msg = resp
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("no error message");
            bail!(
                "the {} kernel rejected a `{op}` request: {msg}",
                self.lang.name()
            );
        }
        Ok(resp)
    }

    fn died(&mut self, op: &str, err: anyhow::Error) -> anyhow::Error {
        let status = match self.child.try_wait() {
            Ok(Some(s)) => format!("it exited ({s})"),
            _ => "it is still running".to_string(),
        };
        anyhow::anyhow!(
            "lost the {} kernel during a `{op}` request ({err}); {status}\n{}",
            self.lang.name(),
            log_tail(&self.log_path)
        )
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        let pids = [self.child.id(), self.kernel_pid];
        KERNEL_PIDS.lock().expect("lock").retain(|p| !pids.contains(p));
        // A launcher that is still running may have a live kernel below it.
        if let Ok(None) = self.child.try_wait() {
            if self.kernel_pid != 0 && self.kernel_pid != self.child.id() {
                unsafe { libc::kill(self.kernel_pid as libc::pid_t, libc::SIGKILL) };
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = std::fs::remove_file(&self.registry_path);
    }
}

fn parse_reply(resp: Value, exec: bool) -> Result<Reply> {
    let failed = resp
        .get("failed")
        .and_then(Value::as_bool)
        .context("kernel reply lacks `failed`")?;
    let array = |k: &str| match resp.get(k) {
        Some(Value::Array(a)) => Ok(a.clone()),
        _ => Err(anyhow::anyhow!("kernel reply lacks `{k}`")),
    };
    let outputs = array("outputs")?;
    let (changed, packages) = if exec {
        let changed = array("changed")?
            .into_iter()
            .map(|v| v.as_str().map(str::to_string).context("`changed` must hold strings"))
            .collect::<Result<_>>()?;
        (changed, array("packages")?)
    } else {
        (Vec::new(), Vec::new())
    };
    let exports = match resp.get("exports") {
        None => Vec::new(),
        Some(_) => array("exports")?
            .iter()
            .map(|e| {
                let field = |k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
                Ok((
                    field("name").context("export without `name`")?,
                    field("file").context("export without `file`")?,
                ))
            })
            .collect::<Result<_>>()?,
    };
    Ok(Reply {
        failed,
        outputs,
        changed,
        packages,
        exports,
    })
}

fn random_token() -> String {
    let s = std::collections::hash_map::RandomState::new();
    let mut a = s.build_hasher();
    a.write_u128(std::process::id() as u128 ^ crate::store::now() as u128);
    let mut b = std::collections::hash_map::RandomState::new().build_hasher();
    b.write_u64(a.finish());
    format!("{:016x}{:016x}", a.finish(), b.finish())
}

fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let tail = &lines[lines.len().saturating_sub(25)..];
    format!(
        "last lines of {}:\n{}",
        path.display(),
        tail.join("\n")
    )
}

/// The kernels of one `loom` process. Kernels are started by the runner on
/// first use and kept here between runs.
#[derive(Default)]
pub struct Kernels {
    map: HashMap<Lang, Kernel>,
}

impl Kernels {
    pub fn is_running(&self, lang: Lang) -> bool {
        self.map.contains_key(&lang)
    }

    /// Position of a session in a running kernel.
    pub fn position(&self, lang: Lang, session: &str) -> Option<&str> {
        self.map.get(&lang).and_then(|k| k.position(session))
    }

    /// Take a running kernel out, to drive it from another thread.
    pub fn take(&mut self, lang: Lang) -> Option<Kernel> {
        self.map.remove(&lang)
    }

    pub fn put(&mut self, kernel: Kernel) {
        self.map.insert(kernel.lang, kernel);
    }

    /// Stop a kernel (after a transport failure, or because its packages
    /// changed), so that the next use starts a new one.
    pub fn discard(&mut self, lang: Lang) {
        self.map.remove(&lang);
    }

    pub fn shutdown(&mut self) {
        for (_, k) in self.map.drain() {
            k.shutdown();
        }
    }
}

/// Registered kernels of a project: `(language file stem, pid, loom pid)`.
pub fn registered(project: &Project) -> Vec<(String, u32, u32)> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(project.kernels_dir()) else {
        return out;
    };
    for f in dir.flatten() {
        let p = f.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(v) = std::fs::read(&p).map_err(anyhow::Error::from).and_then(|b| {
            serde_json::from_slice::<Value>(&b).map_err(anyhow::Error::from)
        }) else {
            continue;
        };
        let pid = v.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
        let loom_pid = v.get("loom_pid").and_then(Value::as_u64).unwrap_or(0) as u32;
        let name = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
        out.push((name, pid, loom_pid));
    }
    out.sort();
    out
}

pub fn pid_alive(pid: u32) -> bool {
    pid != 0 && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
}

/// Exclusive use of a project's `.loom/` directory by one `loom` process.
pub struct ProjectLock {
    path: PathBuf,
}

impl ProjectLock {
    pub fn acquire(project: &Project) -> Result<Self> {
        let dir = project.loom_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("lock");
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    write!(f, "{}", std::process::id())?;
                    *LOCK_PATH.lock().expect("lock") = Some(path.clone());
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let holder: u32 = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0);
                    if pid_alive(holder) {
                        bail!(
                            "another loom process (pid {holder}) is using {}; stop it first",
                            project.display_path(&dir)
                        );
                    }
                    std::fs::remove_file(&path)?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        bail!("cannot lock {}", path.display())
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        *LOCK_PATH.lock().expect("lock") = None;
        let _ = std::fs::remove_file(&self.path);
    }
}
