//! Runs only against an ephemeral local Astraea backend, never production.
use nothingtodo_core::{
    Store, Task,
    sync::{Outcome, SyncClient},
};
use tokio::sync::Mutex;

fn task(title: &str) -> Task {
    Task {
        schema_version: 1,
        title: title.into(),
        notes: "Integration test".into(),
        completed: false,
        remind_at: None,
        extra: Default::default(),
    }
}

fn store() -> Mutex<Store> {
    Mutex::new(Store::open(":memory:").unwrap())
}

async fn sync(client: &SyncClient, store: &Mutex<Store>) -> Result<(), String> {
    while let Outcome::MoreHistory { .. } = client.sync(store).await? {}
    Ok(())
}

#[tokio::test]
#[ignore = "requires scripts/test-sync.mjs and a local Astraea checkout"]
async fn two_devices_conflicts_deletion_pagination_and_offline_retry() {
    let endpoint = std::env::var("NOTHINGTODO_TEST_API").expect("local test API");
    assert!(endpoint.starts_with("http://127.0.0.1:") || endpoint.starts_with("http://localhost:"));
    let token = format!("as_{}", "a".repeat(43));
    let client = SyncClient::new(&endpoint, token.clone()).unwrap();
    let (a, b) = (store(), store());
    let id = a.lock().await.save_task(None, task("Original")).unwrap();
    sync(&client, &a).await.unwrap();
    sync(&client, &b).await.unwrap();
    let title = |s: &Store, id: &str| {
        s.snapshot().records[id]
            .task
            .as_ref()
            .unwrap()
            .title
            .clone()
    };
    assert_eq!(title(&*b.lock().await, &id), "Original");
    a.lock()
        .await
        .save_task(Some(id.clone()), task("Device A"))
        .unwrap();
    b.lock()
        .await
        .save_task(Some(id.clone()), task("Device B"))
        .unwrap();
    sync(&client, &a).await.unwrap();
    sync(&client, &b).await.unwrap();
    assert!(b.lock().await.snapshot().records[&id].conflict.is_some());
    assert_eq!(title(&*b.lock().await, &id), "Device B");
    b.lock().await.resolve(&id, true).unwrap();
    sync(&client, &b).await.unwrap();
    sync(&client, &a).await.unwrap();
    assert_eq!(title(&*a.lock().await, &id), "Device B");
    b.lock().await.delete_task(&id).unwrap();
    sync(&client, &b).await.unwrap();
    sync(&client, &a).await.unwrap();
    assert!(a.lock().await.snapshot().records[&id].task.is_none());

    // An auth failure must not discard offline edits or advance the cursor.
    let queued = a
        .lock()
        .await
        .save_task(None, task("Queued offline"))
        .unwrap();
    let before = a.lock().await.snapshot().cursor;
    let invalid = SyncClient::new(&endpoint, format!("as_{}", "z".repeat(43))).unwrap();
    assert!(invalid.sync(&a).await.is_err());
    assert!(a.lock().await.snapshot().records[&queued].dirty);
    assert_eq!(a.lock().await.snapshot().cursor, before);
    sync(&client, &a).await.unwrap();
    assert!(!a.lock().await.snapshot().records[&queued].dirty);

    // A replaced session's in-flight responses are discarded.
    let replaced = SyncClient::new(&endpoint, token.clone()).unwrap();
    let fresh = store();
    replaced.cancel();
    assert!(
        replaced
            .sync(&fresh)
            .await
            .unwrap_err()
            .contains("replaced")
    );
    assert_eq!(fresh.lock().await.snapshot().cursor, 0);
    assert!(fresh.lock().await.snapshot().binding.is_none());

    // Replay from cursor zero across more than one real server page, one page per pass.
    for n in 0..103 {
        a.lock()
            .await
            .save_task(None, task(&format!("Page item {n}")))
            .unwrap();
    }
    sync(&client, &a).await.unwrap();
    let c = store();
    let paged = SyncClient::new(&endpoint, token)
        .unwrap()
        .with_pages_per_pass(1);
    let mut passes = 0;
    while let Outcome::MoreHistory { cursor } = paged.sync(&c).await.unwrap() {
        assert_eq!(c.lock().await.snapshot().cursor, cursor);
        passes += 1;
    }
    // ~110 events span two server pages, so one pass must stop early.
    assert!(passes >= 1, "expected a bounded pass, got {passes}");
    assert_eq!(c.lock().await.snapshot().records.len(), 105);
    let cursor = c.lock().await.snapshot().cursor;
    sync(&client, &c).await.unwrap();
    assert_eq!(c.lock().await.snapshot().cursor, cursor);
    assert!(
        c.lock()
            .await
            .snapshot()
            .records
            .values()
            .all(|r| !r.dirty && r.conflict.is_none())
    );

    let unbound = store();
    let pending = unbound
        .lock()
        .await
        .save_task(None, task("Unauthorized"))
        .unwrap();
    assert!(invalid.sync(&unbound).await.is_err());
    let unbound = unbound.into_inner().snapshot();
    assert_eq!(unbound.cursor, 0);
    assert!(unbound.binding.is_none());
    assert!(unbound.records[&pending].dirty);
}
