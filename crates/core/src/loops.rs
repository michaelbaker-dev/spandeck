//! The station's periodic work, each kind in a loop of its own.
//!
//! Everything periodic used to be awaited in one loop, twice a second, so
//! one thing that waited held up all the rest: with a station that did not
//! answer and one message queued for it, an agent on this station was told
//! of its mail up to 8.4 s late (measured, 15 Nov 2026). Now each kind of
//! work has its own task and its own clock. The loops marked `local` never
//! wait for the network; what talks to another station does so in a round
//! of that station's own (rounds.rs).
//!
//! A loop whose work panics is logged, counted and started again.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, MissedTickBehavior};

use crate::perf::Perf;
use crate::relay::BoxFuture;

/// The switch (`[flags] split_tick`, or `station.flags`): off, everything
/// periodic runs in one loop again, each thing awaited in turn. Read when
/// the station starts.
pub const SWITCH: &str = "split_tick";

pub struct Loop {
    pub name: &'static str,
    pub every: Duration,
    /// The first run is this long after the start.
    pub first: Duration,
    /// Never waits for the network: its lateness is what "local work is
    /// not held up" is measured by.
    pub local: bool,
}

/// Run `work` every `every`, for good. `work` is given the number of the
/// run, from 1. A run that is still going when the next is due makes that
/// one late (and counted as late); runs never overlap.
pub fn spawn<F>(perf: &Arc<Perf>, l: Loop, work: F) -> tokio::task::JoinHandle<()>
where
    F: Fn(u64) -> BoxFuture<'static, ()> + Send + Sync + 'static,
{
    let stat = perf.loop_stat(l.name, l.every, l.local);
    let perf = perf.clone();
    let (work, n) = (Arc::new(work), Arc::new(AtomicU64::new(0)));
    tokio::spawn(async move {
        let mut first = l.first;
        loop {
            let counted = stat.clone();
            let (work, n, stat, perf) = (work.clone(), n.clone(), stat.clone(), perf.clone());
            let run = tokio::spawn(async move {
                let mut tick = tokio::time::interval_at(Instant::now() + first, l.every);
                // Local work is done on time or not at all; work that waits
                // for something else is done once it can be.
                tick.set_missed_tick_behavior(if l.local {
                    MissedTickBehavior::Skip
                } else {
                    MissedTickBehavior::Delay
                });
                let mut last: Option<Instant> = None;
                loop {
                    tick.tick().await;
                    let began = Instant::now();
                    if let Some(last) = last {
                        let late = began.duration_since(last).saturating_sub(l.every);
                        stat.late.record(late);
                        if l.local {
                            perf.tick_late.record(late);
                        }
                    }
                    last = Some(began);
                    work(n.fetch_add(1, Ordering::Relaxed) + 1).await;
                    stat.work.record(began.elapsed());
                    if l.local {
                        perf.tick_work.record(began.elapsed());
                    }
                }
            });
            match run.await {
                Err(e) if e.is_panic() => {
                    counted.panics.add();
                    tracing::error!(
                        name = l.name,
                        "a periodic task panicked; it is started again"
                    );
                    first = l.every;
                }
                // Cancelled: the runtime is going away.
                _ => return,
            }
        }
    })
}
