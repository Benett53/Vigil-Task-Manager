//! Background sampling thread.
//!
//! The UI thread never touches `/proc` itself: a dedicated thread collects a
//! snapshot at the configured interval, hands it over through a channel and wakes
//! the UI. Between samples egui sleeps, so an idle Vigil costs close to nothing.

use crate::model::Snapshot;
use crate::platform::Collector;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub enum Command {
    SetInterval(Duration),
    SetPaused(bool),
    RefreshNow,
}

pub struct Sampler {
    commands: Sender<Command>,
    snapshots: Receiver<Arc<Snapshot>>,
}

impl Sampler {
    /// Starts the sampler. `wake` is called after each new snapshot (used to repaint the UI).
    pub fn spawn(interval: Duration, paused: bool, wake: impl Fn() + Send + 'static) -> Sampler {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let (snap_tx, snap_rx) = mpsc::channel::<Arc<Snapshot>>();
        std::thread::Builder::new()
            .name("vigil-sampler".into())
            .spawn(move || run(interval, paused, cmd_rx, snap_tx, wake))
            .expect("failed to start sampler thread");
        Sampler {
            commands: cmd_tx,
            snapshots: snap_rx,
        }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }

    /// Returns all snapshots received since the last call, oldest first.
    pub fn drain(&self) -> Vec<Arc<Snapshot>> {
        self.snapshots.try_iter().collect()
    }
}

fn run(
    mut interval: Duration,
    mut paused: bool,
    commands: Receiver<Command>,
    out: Sender<Arc<Snapshot>>,
    wake: impl Fn(),
) {
    let mut collector: Option<Collector> = None;
    let mut failures = 0u32;
    loop {
        if collector.is_none() {
            match catch_unwind(Collector::new) {
                Ok(c) => collector = Some(c),
                Err(_) => {
                    eprintln!("vigil: failed to initialise the system collector");
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
            }
        }
        let started = Instant::now();
        let c = collector.as_mut().expect("initialised above");
        // A bug in a collector must never take the whole application down:
        // recover by starting with a fresh collector.
        match catch_unwind(AssertUnwindSafe(|| c.sample(true))) {
            Ok(snapshot) => {
                failures = 0;
                if out.send(Arc::new(snapshot)).is_err() {
                    return; // UI is gone
                }
                wake();
            }
            Err(_) => {
                failures += 1;
                eprintln!("vigil: sampling failed ({failures}), restarting collector");
                collector = None;
                if failures > 3 {
                    std::thread::sleep(Duration::from_secs(failures.min(30) as u64));
                }
                continue;
            }
        }

        // Wait for the next tick while handling commands.
        let mut deadline = started + interval;
        loop {
            let timeout = if paused {
                Duration::from_secs(3600)
            } else {
                deadline.saturating_duration_since(Instant::now())
            };
            if !paused && timeout.is_zero() {
                break;
            }
            match commands.recv_timeout(timeout) {
                Ok(Command::SetInterval(d)) => {
                    interval = d.max(Duration::from_millis(100));
                    deadline = started + interval;
                }
                Ok(Command::SetPaused(p)) => {
                    paused = p;
                    if !paused {
                        break;
                    }
                }
                Ok(Command::RefreshNow) => break,
                Err(RecvTimeoutError::Timeout) => {
                    if !paused {
                        break;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}
