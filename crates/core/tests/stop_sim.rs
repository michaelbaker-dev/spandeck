//! Risk: Stop when stations lose each other, answer late, or disagree about
//! the time. Stop-all runs over simulated TCP and simulated time (Turmoil),
//! each station with a clock of its own that can be moved.
//!
//! Guarantees checked (DECISIONS.md, "Stop, pause and offline"): three
//! separate states and an unreachable station never shown as stopped; a stop
//! for an unreachable station is delivered on reconnect and applies only to
//! the work that was running when it was issued.
//!
//! Faults these come from (plans/FAILURES.md §7): a station that answered
//! waited 5 s behind one that did not; a request written before a cut was
//! applied 14 s late and stopped a turn started 6 s after the stop.
//!
//! The stations here are the real rows, sending, cut-off and applying-once
//! of `stops.rs`; what stands in for a daemon is a list of panes and when
//! their work began.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use spandeck_core::db::Db;
use spandeck_core::perf::ClockSample;
use spandeck_core::relay::BoxFuture;
use spandeck_core::stops::{self, Hello, StopApplier, StopBook, StopLink, Work};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use turmoil::net::{TcpListener, TcpStream};

fn sim_ms() -> u64 {
    turmoil::sim_elapsed().map_or(0, |d| d.as_millis() as u64)
}

/// Wait until this many milliseconds of simulated time have passed.
async fn until(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms.saturating_sub(sim_ms()))).await;
}

/// A station's clock: simulated time, off from true by an amount that can
/// be stepped.
#[derive(Clone)]
struct Clock(Arc<AtomicI64>);

impl Clock {
    fn off_by(ms: i64) -> Clock {
        Clock(Arc::new(AtomicI64::new(ms)))
    }
    fn now(&self) -> u64 {
        (1_700_000_000_000 + sim_ms() as i64 + self.0.load(Ordering::SeqCst)) as u64
    }
    fn step(&self, by: i64) {
        self.0.fetch_add(by, Ordering::SeqCst);
    }
}

/// A station that is stopped.
struct Target {
    name: &'static str,
    clock: Clock,
    db: Arc<Db>,
    applier: StopApplier,
    panes: Mutex<Vec<(&'static str, Work)>>,
    /// Every interrupt typed into a pane.
    interrupts: Mutex<Vec<&'static str>>,
    hellos: AtomicUsize,
    arrivals: AtomicUsize,
}

impl Target {
    fn new(name: &'static str, clock_off_by: i64) -> Arc<Target> {
        let db = Arc::new(Db::memory().unwrap());
        Arc::new(Target {
            name,
            clock: Clock::off_by(clock_off_by),
            applier: StopApplier::new(db.clone()),
            db,
            panes: Mutex::new(vec![]),
            interrupts: Mutex::new(vec![]),
            hellos: AtomicUsize::new(0),
            arrivals: AtomicUsize::new(0),
        })
    }

    /// A pane's work begins now, by this station's clock. `prompt_hook`:
    /// the agent says when a prompt is submitted; without it only the pane
    /// becoming busy is seen.
    fn begins(&self, pane: &'static str, prompt_hook: bool) -> Work {
        let now = self.clock.now();
        let work = Work {
            busy: true,
            created_ms: 1,
            state_since_ms: now,
            turn_started_ms: if prompt_hook { now } else { 0 },
            busy_since_ms: now,
        };
        self.panes.lock().unwrap().push((pane, work));
        work
    }

    fn interrupted(&self) -> Vec<&'static str> {
        self.interrupts.lock().unwrap().clone()
    }

    fn answer(&self, msg: &Value) -> Value {
        if msg["m"] == json!("hello") {
            self.hellos.fetch_add(1, Ordering::SeqCst);
            return json!({"station": self.name, "now_ms": self.clock.now(), "caps": [stops::CAP]});
        }
        self.arrivals.fetch_add(1, Ordering::SeqCst);
        let p = &msg["p"];
        let aim = stops::aim(
            self.clock.now(),
            p["issued_ms"].as_u64().unwrap(),
            p["age_ms"].as_u64(),
            p["cutoff_ms"].as_u64(),
        );
        let (answer, _) = self.applier.apply(p["id"].as_str().unwrap(), || {
            let mut stopped = vec![];
            for (pane, work) in self.panes.lock().unwrap().iter() {
                if stops::hit(work, &aim) {
                    self.interrupts.lock().unwrap().push(pane);
                    stopped.push(*pane);
                }
            }
            json!({"stopped": stopped, "station": self.name})
        });
        answer
    }
}

async fn serve(t: Arc<Target>) -> turmoil::Result {
    let listener = TcpListener::bind("0.0.0.0:7391").await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let t = t.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            if BufReader::new(read).read_line(&mut line).await.is_err() {
                return;
            }
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                return;
            };
            let _ = write
                .write_all(format!("{}\n", t.answer(&msg)).as_bytes())
                .await;
        });
    }
}

/// The issuing station's side of the link: one JSON line each way. A
/// request that was written stays on its way when the caller gives up, as
/// a request in a real connection does.
struct SimLink {
    clock: Clock,
    samples: Mutex<HashMap<String, Vec<ClockSample>>>,
    listed: Mutex<HashMap<String, String>>,
}

async fn call(station: &str, msg: Value) -> Result<Value, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let station = station.to_string();
    tokio::spawn(async move {
        let reply = async {
            let stream = TcpStream::connect((station.as_str(), 7391))
                .await
                .map_err(|e| e.to_string())?;
            let (read, mut write) = stream.into_split();
            write
                .write_all(format!("{msg}\n").as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            let mut line = String::new();
            BufReader::new(read)
                .read_line(&mut line)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::from_str::<Value>(&line).map_err(|e| format!("no reply: {e}"))
        }
        .await;
        let _ = tx.send(reply);
    });
    rx.await
        .unwrap_or_else(|_| Err("gone".into()))
        .map_err(|e| format!("unreachable: {e}"))
}

impl SimLink {
    fn new() -> SimLink {
        SimLink {
            clock: Clock::off_by(0),
            samples: Mutex::new(HashMap::new()),
            listed: Mutex::new(HashMap::new()),
        }
    }

    /// What a look at the fleet does for one station: it answers, is listed
    /// as online, and its clock is read.
    async fn look(&self, station: &str) {
        self.hello(station).await.unwrap();
        self.list(station, "online");
    }

    fn list(&self, station: &str, state: &str) {
        self.listed
            .lock()
            .unwrap()
            .insert(station.to_string(), state.to_string());
    }
}

impl StopLink for SimLink {
    fn now_ms(&self) -> u64 {
        self.clock.now()
    }

    fn samples(&self, station: &str) -> (Vec<ClockSample>, u64) {
        let samples = self.samples.lock().unwrap();
        (
            samples.get(station).cloned().unwrap_or_default(),
            sim_ms() / 1000,
        )
    }

    fn peer(&self, station: &str) -> Option<(String, bool)> {
        let listed = self.listed.lock().unwrap();
        listed.get(station).map(|state| (state.clone(), true))
    }

    fn hello<'a>(&'a self, station: &'a str) -> BoxFuture<'a, Result<Hello, String>> {
        Box::pin(async move {
            let (sent, began) = (self.clock.now(), sim_ms());
            let h = call(station, json!({"m": "hello"})).await?;
            let rtt_ms = sim_ms() - began;
            let theirs = h["now_ms"].as_u64();
            if let Some(theirs) = theirs {
                let mut samples = self.samples.lock().unwrap();
                samples
                    .entry(station.to_string())
                    .or_default()
                    .push(ClockSample {
                        at_sec: sim_ms() / 1000,
                        at_ms: self.clock.now(),
                        offset_ms: theirs as i64 - (sent + rtt_ms / 2) as i64,
                        rtt_ms,
                    });
            }
            Ok(Hello {
                station: h["station"].as_str().map(String::from),
                now_ms: theirs,
                cutoff: h["caps"]
                    .as_array()
                    .is_some_and(|c| c.contains(&json!(stops::CAP))),
            })
        })
    }

    fn apply<'a>(
        &'a self,
        station: &'a str,
        params: Value,
    ) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(call(station, json!({"m": "apply", "p": params})))
    }
}

fn sim() -> turmoil::Sim<'static> {
    turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(120))
        .tick_duration(Duration::from_millis(5))
        .min_message_latency(Duration::from_millis(20))
        .max_message_latency(Duration::from_millis(20))
        .rng_seed(1)
        .build()
}

fn row(book: &StopBook, station: &str) -> Value {
    book.list(50, 0)
        .into_iter()
        .find(|r| r["station"] == json!(station))
        .unwrap_or(Value::Null)
}

/// A fleet stop with one station that does not answer and one that cannot
/// be reached: the one that answers is stopped at once, not behind the
/// others (measured: 5.01 s behind one). A station not reached is
/// "unreachable", never "stopped"; a state never goes back; and the stop is
/// delivered when the station returns, by an issuing station that has
/// restarted since, aimed at the moment the stop was issued.
#[test]
fn a_fleet_stop_waits_for_no_station_and_reaches_the_one_that_was_away() {
    let mut sim = sim();
    let (bravo, charlie, delta) = (
        Target::new("bravo", 90_000),
        Target::new("charlie", -3_000),
        Target::new("delta", 500),
    );
    for t in [&bravo, &charlie, &delta] {
        let t = t.clone();
        sim.host(t.name, move || serve(t.clone()));
    }
    sim.client("alpha", async move {
        let link = SimLink::new();
        let db = Arc::new(Db::memory().unwrap());
        for at in [0, 1000] {
            until(at).await;
            for t in ["bravo", "charlie", "delta"] {
                link.look(t).await;
            }
        }
        for t in [&bravo, &charlie, &delta] {
            t.begins("running", true);
        }
        until(3000).await;
        // charlie takes the connection and says nothing; delta is cut off.
        turmoil::hold("alpha", "charlie");
        turmoil::partition("alpha", "delta");

        let book = Arc::new(StopBook::new(db.clone(), "alpha"));
        let targets = ["bravo", "charlie", "delta"].map(String::from);
        let ids = book.issue(&targets, "fleet", "you", link.now_ms());
        assert!(book.save());
        let sending = {
            let (book, ids) = (book.clone(), ids.clone());
            let link = &link;
            async move {
                spandeck_core::fleet::futures_join_all(
                    ids.iter().map(|id| stops::send(&book, link, id, true)),
                )
                .await
            }
        };
        let check = async {
            until(3300).await;
            // 0.3 s on: the one that answers is done, the one that cannot
            // be reached is known to be so, the silent one is still waited for.
            assert_eq!(row(&book, "bravo")["status"], json!("confirmed"));
            assert_eq!(bravo.interrupted(), ["running"]);
            assert_eq!(row(&book, "charlie")["status"], json!("requested"));
            assert_eq!(row(&book, "delta")["status"], json!("unreachable"));
        };
        tokio::join!(sending, check);
        assert_eq!(sim_ms(), 8000, "charlie's own five seconds, and no more");
        for away in ["charlie", "delta"] {
            let waiting = row(&book, away);
            assert_eq!(waiting["status"], json!("unreachable"), "{waiting}");
            assert_eq!(waiting["label"], json!("station unreachable"));
        }
        assert!(charlie.interrupted().is_empty() && delta.interrupted().is_empty());

        // A failure that comes in after a station confirmed changes nothing.
        assert!(!book.mark(&ids[0], "unreachable", json!({"error": "late"}), 0));
        assert!(!book.mark(&ids[0], "requested", json!({}), 0));
        assert!(book.save());
        assert_eq!(row(&book, "bravo")["status"], json!("confirmed"));

        // The issuing station restarts; delta starts new work; delta is back
        // 6 s after the stop, while the old clock readings are still fresh.
        drop(book);
        let book = StopBook::new(db.clone(), "alpha");
        book.load_waiting();
        assert_eq!(
            book.waiting()
                .iter()
                .map(|r| r.station.as_str())
                .collect::<Vec<_>>(),
            ["charlie", "delta"]
        );
        link.list("charlie", "unreachable");
        link.list("delta", "unreachable");
        assert!(stops::deliver_waiting(&book, &link, true)
            .await
            .tried
            .is_empty());
        delta.begins("started after the stop", true);
        until(9000).await;
        turmoil::repair("alpha", "delta");
        link.list("delta", "online");
        assert!(
            stops::standing(&link.samples("delta").0, sim_ms() / 1000, link.now_ms()).is_some(),
            "the standing reading alone would pass"
        );
        let hellos = delta.hellos.load(Ordering::SeqCst);
        let round = stops::deliver_waiting(&book, &link, true).await;
        assert_eq!(round.tried, [ids[2].clone()]);
        assert_eq!(
            delta.hellos.load(Ordering::SeqCst),
            hellos + 1,
            "a station that was away is asked its clock before it is stopped"
        );
        let done = row(&book, "delta");
        assert_eq!(done["status"], json!("confirmed"), "{done}");
        assert_eq!(done["result"]["stopped"], json!(["running"]));
        assert_eq!(delta.interrupted(), ["running"]);
        assert!(book.save());
        // charlie's still waits, and is still in the database.
        let kept = StopBook::new(db, "alpha");
        assert_eq!(row(&kept, "charlie")["status"], json!("unreachable"));
        assert_eq!(row(&kept, "delta")["status"], json!("confirmed"));
        assert!(kept.list(50, 0).iter().all(|r| r["saved"] == json!(true)));
        Ok(())
    });
    sim.run().unwrap();
}

/// A stop that is 14 s on the way stops what was running when it was issued
/// and leaves alone a turn started 6 s after it (measured: that turn was
/// stopped, in 2 of 6 runs). Its retry is the same stop: applied once. And
/// a clock that stepped between two readings is asked again, so the cut-off
/// is in the clock the station has now.
#[test]
fn a_late_stop_is_aimed_at_the_moment_it_was_issued() {
    let mut sim = sim();
    let (bravo, charlie) = (Target::new("bravo", 90_000), Target::new("charlie", -3_000));
    for t in [&bravo, &charlie] {
        let t = t.clone();
        sim.host(t.name, move || serve(t.clone()));
    }
    sim.client("alpha", async move {
        let link = SimLink::new();
        let book = StopBook::new(Arc::new(Db::memory().unwrap()), "alpha");
        for at in [0, 1000] {
            until(at).await;
            link.look("bravo").await;
            link.look("charlie").await;
        }
        bravo.begins("running", true);
        bravo.begins("no prompt hook", false);
        bravo.panes.lock().unwrap().push((
            "start not known",
            Work {
                busy: true,
                created_ms: 1,
                state_since_ms: 0,
                turn_started_ms: 0,
                busy_since_ms: 0,
            },
        ));
        until(3000).await;
        turmoil::hold("alpha", "bravo");
        let issued = link.now_ms();
        let ids = book.issue(&["bravo".to_string()], "bravo", "you", issued);
        stops::send(&book, &link, &ids[0], true).await;
        assert_eq!(row(&book, "bravo")["status"], json!("unreachable"));
        until(9000).await;
        let later = bravo.begins("started 6 s after the stop", true);
        bravo.begins("busy 6 s after the stop, no prompt hook", false);
        until(17_000).await;
        assert_eq!(bravo.arrivals.load(Ordering::SeqCst), 0);
        turmoil::release("alpha", "bravo");
        until(17_500).await;
        assert_eq!(bravo.arrivals.load(Ordering::SeqCst), 1, "14 s late");
        let hit = ["running", "no prompt hook", "start not known"];
        assert_eq!(bravo.interrupted(), hit);
        // Counting back by its age from when it arrived, as before, takes
        // the later turn for one that was running.
        assert!(stops::hit(
            &later,
            &stops::aim(bravo.clock.now(), issued, Some(0), None)
        ));

        // The retry: the same stop, the same answer, nothing typed twice.
        link.list("bravo", "online");
        stops::deliver_waiting(&book, &link, true).await;
        assert_eq!(bravo.arrivals.load(Ordering::SeqCst), 2);
        assert_eq!(bravo.interrupted(), hit);
        let done = row(&book, "bravo");
        assert_eq!(done["status"], json!("confirmed"));
        assert_eq!(done["result"]["stopped"], json!(hit));

        // charlie's clock is put forward 30 s. The next look reads it; the
        // two newest readings now disagree.
        until(20_000).await;
        charlie.clock.step(30_000);
        until(21_000).await;
        link.look("charlie").await;
        let (samples, now_sec) = link.samples("charlie");
        assert_eq!(samples.len(), 3);
        assert_eq!(stops::standing(&samples, now_sec, link.now_ms()), None);
        until(21_500).await;
        charlie.begins("running", true);
        until(22_000).await;
        let hellos = charlie.hellos.load(Ordering::SeqCst);
        let ids = book.issue(&["charlie".to_string()], "charlie", "you", link.now_ms());
        let sending = stops::send(&book, &link, &ids[0], true);
        let meanwhile = async {
            // After the stop was issued, before it is there.
            until(22_100).await;
            charlie.begins("started after", true);
        };
        tokio::join!(sending, meanwhile);
        assert_eq!(charlie.hellos.load(Ordering::SeqCst), hellos + 1);
        assert_eq!(row(&book, "charlie")["status"], json!("confirmed"));
        assert_eq!(charlie.interrupted(), ["running"]);
        Ok(())
    });
    sim.run().unwrap();
}

/// A database that takes no writes, at the issuing station and at the one
/// stopped: the stop is applied all the same, the row says it is not saved
/// until it is, and a second arrival is still not a second stop.
#[test]
fn a_stop_is_applied_when_the_database_takes_no_writes() {
    let mut sim = sim();
    let bravo = Target::new("bravo", 0);
    {
        let t = bravo.clone();
        sim.host("bravo", move || serve(t.clone()));
    }
    sim.client("alpha", async move {
        let link = SimLink::new();
        let db = Arc::new(Db::memory().unwrap());
        let book = StopBook::new(db.clone(), "alpha");
        let rename = |db: &Db, from: &str, to: &str| {
            db.with(|c| c.execute_batch(&format!("ALTER TABLE {from} RENAME TO {to}")))
                .unwrap()
        };
        rename(&db, "stops", "stops_away");
        rename(&bravo.db, "kv", "kv_away");
        link.look("bravo").await;
        bravo.begins("running", true);
        until(1000).await;

        let ids = book.issue(&["bravo".to_string()], "bravo", "you", link.now_ms());
        assert!(!book.save());
        assert_eq!(row(&book, "bravo")["saved"], json!(false));
        stops::send(&book, &link, &ids[0], true).await;
        assert!(!book.save());
        let done = row(&book, "bravo");
        assert_eq!(
            (&done["status"], &done["saved"]),
            (&json!("confirmed"), &json!(false)),
            "{done}"
        );
        assert_eq!(bravo.interrupted(), ["running"]);

        // The same stop again, and bravo could not write that it had it.
        until(2000).await;
        bravo.begins("started after", true);
        let again = link
            .apply(
                "bravo",
                json!({"id": ids[0], "issued_ms": link.now_ms(), "age_ms": 0}),
            )
            .await
            .unwrap();
        assert_eq!(again["stopped"], json!(["running"]));
        assert_eq!(bravo.interrupted(), ["running"]);

        // The database is back: the row is written, as it now stands.
        rename(&db, "stops_away", "stops");
        assert!(book.save());
        let kept = row(&StopBook::new(db, "alpha"), "bravo");
        assert_eq!(
            (&kept["status"], &kept["saved"]),
            (&json!("confirmed"), &json!(true)),
            "{kept}"
        );
        Ok(())
    });
    sim.run().unwrap();
}
