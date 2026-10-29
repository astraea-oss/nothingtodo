//! Performance baseline (roadmap 0a). Run with `npm run baseline`, which starts a throwaway local
//! backend; never point it at production. Prints a table to compare against later changes.
use nothingtodo_core::{
    Store, Task, backup,
    sync::{Outcome, SyncClient},
};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

fn task(i: usize) -> Task {
    Task {
        schema_version: 1,
        title: format!("Baseline task {i} with a realistic length of title"),
        notes: "A line or two of notes, as most tasks have.".into(),
        completed: i.is_multiple_of(3),
        remind_at: Some(1_900_000_000_000 + i as i64 * 60_000),
        kind: None,
        date: None,
        duration_minutes: None,
        repeat: None,
        repeat_from: None,
        time_zone: None,
        warnings: vec![],
        extra: Default::default(),
    }
}

/// A store with `n` tasks, loaded in one commit through a restore (per-task saves are quadratic
/// in the snapshot model, which is part of what this baseline measures).
fn loaded(path: &std::path::Path, n: usize) -> Store {
    let mut seed = Store::open(":memory:").unwrap();
    let mut snapshot = seed.snapshot();
    for i in 0..n {
        snapshot.records.insert(
            uuid::Uuid::new_v4().to_string(),
            nothingtodo_core::Record {
                task: Some(task(i)),
                version: 1,
                ..Default::default()
            },
        );
    }
    let json = serde_json::json!({
        "format": backup::FORMAT, "formatVersion": backup::FORMAT_VERSION,
        "exportedAt": 0, "snapshot": snapshot,
    });
    let mut store = Store::open(path).unwrap();
    store
        .restore(backup::parse(&json.to_string()).unwrap())
        .unwrap();
    drop(seed.alarms(0));
    store
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "run with npm run baseline"]
fn local_store() {
    println!("\n| tasks | snapshot KiB | open ms | save ms (median of 20) | alarm list ms |");
    println!("|---|---|---|---|---|");
    for n in [100, 1_000, 5_000] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("baseline.sqlite3");
        let mut store = loaded(&path, n);
        let bytes = serde_json::to_vec(&store.snapshot()).unwrap().len();
        let id = store.snapshot().records.keys().next().unwrap().clone();
        let saves = (0..20)
            .map(|i| {
                let started = Instant::now();
                store.save_task(Some(id.clone()), task(i)).unwrap();
                started.elapsed()
            })
            .collect();
        let started = Instant::now();
        store.alarms(0).unwrap();
        let alarms = started.elapsed();
        drop(store);
        let started = Instant::now();
        drop(Store::open(&path).unwrap());
        let open = started.elapsed();
        println!(
            "| {n} | {} | {:.1} | {:.2} | {:.2} |",
            bytes / 1024,
            open.as_secs_f64() * 1e3,
            median(saves).as_secs_f64() * 1e3,
            alarms.as_secs_f64() * 1e3
        );
    }
}

#[tokio::test]
#[ignore = "run with npm run baseline"]
async fn first_sync() {
    let endpoint = std::env::var("NOTHINGTODO_TEST_API").expect("local test API");
    assert!(endpoint.starts_with("http://127.0.0.1:") || endpoint.starts_with("http://localhost:"));
    let client = SyncClient::new(&endpoint, format!("as_{}", "a".repeat(43))).unwrap();
    let n = 1_000;
    let source = Mutex::new(Store::open(":memory:").unwrap());
    for i in 0..n {
        source.lock().await.save_task(None, task(i)).unwrap();
    }
    let started = Instant::now();
    while let Outcome::MoreHistory { .. } = client.sync(&source).await.unwrap() {}
    let upload = started.elapsed();

    let fresh = Mutex::new(Store::open(":memory:").unwrap());
    let started = Instant::now();
    let mut passes = 1;
    while let Outcome::MoreHistory { .. } = client.sync(&fresh).await.unwrap() {
        passes += 1;
    }
    let download = started.elapsed();
    // An edit made right after launch waits for at most one page commit, not the whole replay.
    let tasks = fresh.lock().await.snapshot().records.len();
    assert_eq!(tasks, n);
    println!("\n| events | upload s (one PUT each) | first sync s | passes (20 pages each) |");
    println!("|---|---|---|---|");
    println!(
        "| {n} | {:.1} | {:.2} | {passes} |",
        upload.as_secs_f64(),
        download.as_secs_f64()
    );
}
