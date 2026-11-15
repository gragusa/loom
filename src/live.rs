//! State shared between a run and the threads that may cut it short: the
//! file watcher of `loom watch` and the signal handler.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A cell a kernel is computing.
pub struct Running {
    pub pid: u32,
    pub key: String,
}

#[derive(Default)]
pub struct Live {
    /// A source changed since the current run started.
    pub dirty: AtomicBool,
    /// Cell keys changed: the run stops at the next cell boundary.
    pub abort: AtomicBool,
    /// Cache keys of the plan being run.
    pub keys: Mutex<HashSet<String>>,
    /// Cells being computed, one per language at most.
    pub running: Mutex<Vec<Running>>,
    /// Keys of cells that were sent SIGINT; their results are discarded.
    interrupted: Mutex<HashSet<String>>,
    last_interrupt: Mutex<Option<Instant>>,
}

impl Live {
    /// Send SIGINT to the kernels computing cells that `select` accepts.
    /// Returns whether any cell was interrupted.
    fn interrupt_where(&self, select: impl Fn(&Running) -> bool) -> bool {
        let running = self.running.lock().expect("lock");
        let mut any = false;
        for r in running.iter().filter(|r| select(r)) {
            self.interrupted.lock().expect("lock").insert(r.key.clone());
            unsafe { libc::kill(r.pid as libc::pid_t, libc::SIGINT) };
            any = true;
        }
        if any {
            *self.last_interrupt.lock().expect("lock") = Some(Instant::now());
        }
        any
    }

    /// Interrupt every cell being computed.
    pub fn interrupt_running(&self) -> bool {
        self.interrupt_where(|_| true)
    }

    /// Whether the cell with `key` was interrupted; clears the mark.
    pub fn take_interrupted(&self, key: &str) -> bool {
        self.interrupted.lock().expect("lock").remove(key)
    }

    /// Whether an interrupt was sent within `window`; a Ctrl-C that follows
    /// one quickly means "quit".
    pub fn interrupted_within(&self, window: Duration) -> bool {
        self.last_interrupt
            .lock()
            .expect("lock")
            .is_some_and(|t| t.elapsed() < window)
    }

    /// Called when a source changes: compare the keys of the new plan with
    /// those being run. Changed keys stop the run at the next cell; cells
    /// being computed that are no longer part of the document are
    /// interrupted.
    pub fn source_changed(&self, new_keys: Option<HashSet<String>>) {
        self.dirty.store(true, Ordering::SeqCst);
        let Some(new_keys) = new_keys else { return };
        if *self.keys.lock().expect("lock") == new_keys {
            return;
        }
        self.abort.store(true, Ordering::SeqCst);
        self.interrupt_where(|r| !new_keys.contains(&r.key));
    }
}
