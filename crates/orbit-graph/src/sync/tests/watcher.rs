use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use notify::{Event, EventKind};

use super::{event_loop, forward_event, spawn_stoppable, stop_and_join};

fn source_event() -> notify::Result<Event> {
    Ok(Event::new(EventKind::Any).add_path("/repo/src/lib.rs".into()))
}

#[test]
fn full_event_channel_drops_and_counts_events_without_blocking() {
    let (event_tx, event_rx) = mpsc::sync_channel(1);
    let dropped = AtomicU64::new(0);

    forward_event(&event_tx, &dropped, source_event());
    forward_event(&event_tx, &dropped, source_event());
    forward_event(&event_tx, &dropped, source_event());

    assert_eq!(dropped.load(Ordering::Relaxed), 2);
    assert!(event_rx.try_recv().is_ok());
    assert!(event_rx.try_recv().is_err(), "the channel held one event");
}

#[test]
fn dropped_events_schedule_a_full_rescan() {
    // No event is ever delivered: only the overflow count can trigger a sync.
    let (_event_tx, event_rx) = mpsc::sync_channel::<notify::Result<Event>>(1);
    let dropped = Arc::new(AtomicU64::new(1));
    let stop = Arc::new(AtomicBool::new(false));
    let (synced_tx, synced_rx) = mpsc::sync_channel(1);

    let loop_dropped = Arc::clone(&dropped);
    let loop_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        let mut run_sync = || {
            let _ = synced_tx.try_send(());
            loop_stop.store(true, Ordering::Relaxed);
        };
        event_loop(
            &event_rx,
            &loop_dropped,
            &loop_stop,
            Path::new("/repo"),
            Duration::ZERO,
            &mut run_sync,
        );
    });

    let synced = synced_rx.recv_timeout(Duration::from_secs(10));
    stop.store(true, Ordering::Relaxed);
    handle.join().expect("event loop thread");
    assert!(synced.is_ok(), "an overflow must schedule a rescan");
    assert_eq!(dropped.load(Ordering::Relaxed), 0, "the count was consumed");
}

#[test]
fn event_loop_stops_promptly_despite_a_long_debounce() {
    let (event_tx, event_rx) = mpsc::sync_channel(1);
    event_tx.send(source_event()).expect("queue event");
    let dropped = AtomicU64::new(0);
    let stop = Arc::new(AtomicBool::new(false));
    let loop_stop = Arc::clone(&stop);
    let (finished, handle) = spawn_stoppable("event-loop-test", move || {
        event_loop(
            &event_rx,
            &dropped,
            &loop_stop,
            Path::new("/repo"),
            Duration::from_secs(3600),
            &mut || panic!("the debounce never elapses"),
        );
    })
    .expect("spawn event loop");

    assert!(
        stop_and_join(&stop, &finished, handle, Duration::from_secs(10)),
        "a pending hour-long debounce must not delay stopping"
    );
}

#[test]
fn stop_and_join_detaches_a_thread_that_misses_the_deadline() {
    let stop = AtomicBool::new(false);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let thread_release = Arc::clone(&release);
    let (finished, handle) = spawn_stoppable("stuck-watcher-test", move || {
        let (lock, cvar) = thread_release.as_ref();
        let released = lock.lock().expect("release lock");
        let _released = cvar
            .wait_while(released, |released| !*released)
            .expect("wait for release");
    })
    .expect("spawn stuck thread");

    let started = Instant::now();
    let joined = stop_and_join(&stop, &finished, handle, Duration::from_millis(20));
    let waited = started.elapsed();

    assert!(!joined, "a stuck thread is detached, not joined");
    assert!(stop.load(Ordering::Relaxed), "the thread was asked to stop");
    assert!(waited < Duration::from_secs(5), "join overran: {waited:?}");

    let (lock, cvar) = release.as_ref();
    *lock.lock().expect("release lock") = true;
    cvar.notify_all();
    let (done_lock, done_cvar) = finished.as_ref();
    let done = done_lock.lock().expect("finished lock");
    let (done, _) = done_cvar
        .wait_timeout_while(done, Duration::from_secs(10), |done| !*done)
        .expect("wait for detached thread");
    assert!(*done, "the detached thread still finishes once released");
}
