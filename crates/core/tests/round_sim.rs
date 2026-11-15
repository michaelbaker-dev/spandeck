//! Risk: one station that is slow or away slows everyone else down.
//!
//! Measured on 15 Nov 2026, before this: with one station that did not
//! answer and one message queued for it, an agent on the same station as
//! its sender was told of its mail up to 8.4 s late, because everything
//! periodic was awaited in one loop. The owner's rule: "the slow or absent
//! station has to be fixed with the right architecture for a system like
//! this, never to slow everyone else down."
//!
//! The rounds and the loops run here over simulated TCP and simulated time
//! (Turmoil): no wall clock is read and none is asserted on.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use spandeck_core::loops::{self, Loop};
use spandeck_core::perf::Perf;
use spandeck_core::rounds::{BoxFuture, Kind, Link, Listed, Outcome, Rounds, Select};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use turmoil::net::{TcpListener, TcpStream};

/// A round over simulated TCP: one line each way. It has no time limit of
/// its own: the deadline is the rounds'.
#[derive(Clone)]
struct SimLink;

impl Link for SimLink {
    fn round<'a>(&'a self, addr: &'a str) -> BoxFuture<'a, Outcome> {
        Box::pin(async move {
            let Ok(stream) = TcpStream::connect((addr, 7000)).await else {
                return Outcome::Silent;
            };
            let (read, mut write) = stream.into_split();
            if write.write_all(b"round\n").await.is_err() {
                return Outcome::Silent;
            }
            let mut line = String::new();
            match BufReader::new(read).read_line(&mut line).await {
                Ok(n) if n > 0 => Outcome::Answered,
                _ => Outcome::Silent,
            }
        })
    }
}

/// What a simulated station counted.
#[derive(Default)]
struct Station {
    /// Rounds that reached it.
    asked: AtomicU64,
    /// Rounds with it that are going on now, and the most there ever were.
    open: AtomicU64,
    most_open: AtomicU64,
    /// It reads the question and never answers.
    hung: AtomicBool,
}

/// A station that answers after `delay`, or (while `hung`) never.
async fn station(s: Arc<Station>, delay: Duration) -> turmoil::Result {
    let listener = TcpListener::bind("0.0.0.0:7000").await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let s = s.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            let mut line = String::new();
            if read.read_line(&mut line).await.is_err() {
                return;
            }
            s.asked.fetch_add(1, SeqCst);
            let open = s.open.fetch_add(1, SeqCst) + 1;
            s.most_open.fetch_max(open, SeqCst);
            if s.hung.load(SeqCst) {
                // Held until the asker gives up and closes.
                line.clear();
                let _ = read.read_line(&mut line).await;
            } else {
                tokio::time::sleep(delay).await;
                let _ = write.write_all(b"here\n").await;
            }
            s.open.fetch_sub(1, SeqCst);
        });
    }
}

fn sim() -> turmoil::Sim<'static> {
    turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(600))
        .tick_duration(Duration::from_millis(10))
        .build()
}

#[test]
fn a_station_that_never_answers_delays_only_its_own_rounds() {
    let mut sim = sim();
    let (bravo, charlie, delta) = (
        Arc::new(Station::default()),
        Arc::new(Station::default()),
        Arc::new(Station::default()),
    );
    delta.hung.store(true, SeqCst);
    let ms = Duration::from_millis;
    for (name, s, delay) in [
        ("bravo", &bravo, ms(20)),
        // Slow, as a station behind a poor link is: it answers.
        ("charlie", &charlie, ms(900)),
        ("delta", &delta, ms(20)),
    ] {
        let s = s.clone();
        sim.host(name, move || station(s.clone(), delay));
    }
    let (b, c, d) = (bravo.clone(), charlie.clone(), delta.clone());
    sim.client("alpha", async move {
        let secs = Duration::from_secs;
        let perf = Arc::new(Perf::default());
        // The deadline a real round has (fleet.rs).
        let rounds = Rounds::new(secs(8));
        let listed: Vec<Listed> = ["bravo", "charlie", "delta"]
            .iter()
            .map(|addr| Listed {
                addr: addr.to_string(),
                kind: Kind::Member,
                online: None,
            })
            .collect();
        // The look: every ten seconds it starts the rounds that are due,
        // and waits for none of them.
        let (r, l) = (rounds.clone(), listed.clone());
        let every = |name, every| Loop {
            name,
            every,
            first: Duration::ZERO,
            local: name != "fleet",
        };
        loops::spawn(&perf, every("fleet", secs(10)), move |_| {
            r.follow(&l);
            r.kick(&SimLink, Select::Due);
            Box::pin(async {})
        });
        // Local work, as the mail wake-ups are: once a second.
        let woke = Arc::new(AtomicU64::new(0));
        let w = woke.clone();
        loops::spawn(&perf, every("mail", secs(1)), move |_| {
            w.fetch_add(1, SeqCst);
            Box::pin(async {})
        });

        tokio::time::sleep(ms(65_500)).await;
        // Local work ran every second of the 65, the hung station or not.
        let ran = woke.load(SeqCst);
        assert!((65..=66).contains(&ran), "local work ran {ran} times");
        // The stations that answer had every one of their rounds, at 0,
        // 10 … 60 s: seven each, the slow one too.
        assert_eq!(b.asked.load(SeqCst), 7, "the station that answers");
        assert_eq!(c.asked.load(SeqCst), 7, "the slow station");
        // The one that never answers: each round cut off at 8 s, never two
        // at once, and after three failures asked less often (0, 10, 20,
        // then 40 s; the next would be at 70 s).
        assert_eq!(d.asked.load(SeqCst), 4, "the hung station is backed off");
        assert_eq!(d.most_open.load(SeqCst), 1, "its rounds do not pile up");
        let view = rounds.view();
        assert_eq!(view["delta"]["failed_in_a_row"], 4, "{view:?}");
        assert_eq!(view["bravo"]["failed_in_a_row"], 0, "{view:?}");

        // A caller that waits for the fleet (the owner's change to a
        // synced document) waits for the members asked, for a bounded
        // time: here the hung station's round is under way.
        tokio::time::sleep(ms(5_000)).await;
        assert_eq!(d.asked.load(SeqCst), 5, "its round at 70 s began");
        let began = tokio::time::Instant::now();
        rounds.settle(true, secs(5)).await;
        let waited = began.elapsed();
        assert!(waited >= secs(5) && waited < ms(5_100), "{waited:?}");

        // It answers again and is heard from: asked at once, not at its
        // backed-off turn (70 s + 30 s), and then at every look again.
        tokio::time::sleep(secs(5)).await;
        d.hung.store(false, SeqCst);
        assert!(rounds.heard("delta"), "a station heard from is due");
        assert!(!rounds.heard("bravo"), "one that answers is left alone");
        assert_eq!(rounds.kick(&SimLink, Select::Due), 1);
        rounds.settle(true, secs(5)).await;
        assert_eq!(rounds.view()["delta"]["failed_in_a_row"], 0);
        let before = d.asked.load(SeqCst);
        tokio::time::sleep(secs(30)).await;
        assert_eq!(d.asked.load(SeqCst) - before, 3, "every look again");
        Ok(())
    });
    sim.run().unwrap();
    assert_eq!(bravo.most_open.load(SeqCst), 1);
    assert_eq!(charlie.most_open.load(SeqCst), 1);
}

/// Unit 07, item 2: one loop used to carry everything, so a panic in any
/// part of it ended all periodic work for good. A loop whose work panics
/// is started again and counted, and the others never notice.
#[test]
fn a_loop_whose_work_panics_is_started_again() {
    let mut sim = sim();
    sim.client("alpha", async move {
        let perf = Arc::new(Perf::default());
        let every = |name| Loop {
            name,
            every: Duration::from_secs(1),
            first: Duration::ZERO,
            local: true,
        };
        let (bad, good) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let b = bad.clone();
        loops::spawn(&perf, every("bad"), move |n| {
            b.fetch_add(1, SeqCst);
            assert!(n != 3, "the third run fails (this panic is the test's)");
            Box::pin(async {})
        });
        let g = good.clone();
        loops::spawn(&perf, every("good"), move |_| {
            g.fetch_add(1, SeqCst);
            Box::pin(async {})
        });
        tokio::time::sleep(Duration::from_millis(10_500)).await;
        // Runs at 0, 1 and 2 s (the third panics), then every second from
        // 3 s on: its numbering goes on where it was.
        assert_eq!(bad.load(SeqCst), 11);
        assert_eq!(good.load(SeqCst), 11);
        let stat = perf.loop_stat("bad", Duration::from_secs(1), true);
        assert_eq!(stat.panics.total(), 1);
        Ok(())
    });
    sim.run().unwrap();
}
