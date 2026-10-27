//! Runs only against an ephemeral local Astraea backend, never production.
use nothingtodo_core::{Store, Task, sync::SyncClient};

fn task(title: &str) -> Task {
    Task {
        schema_version: 1,
        title: title.into(),
        notes: "Integration test".into(),
        completed: false,
        remind_at: None,
    }
}

#[tokio::test]
#[ignore = "requires scripts/test-sync.mjs and a local Astraea checkout"]
async fn two_devices_conflicts_deletion_pagination_and_offline_retry() {
    let endpoint = std::env::var("NOTHINGTODO_TEST_API").expect("local test API");
    assert!(endpoint.starts_with("http://127.0.0.1:") || endpoint.starts_with("http://localhost:"));
    let token = format!("as_{}", "a".repeat(43));
    let client = SyncClient::new(&endpoint, token).unwrap();
    let mut a = Store::open(":memory:").unwrap();
    let mut b = Store::open(":memory:").unwrap();
    let id = a.save_task(None, task("Original")).unwrap();
    client.sync(&mut a).await.unwrap();
    client.sync(&mut b).await.unwrap();
    assert_eq!(
        b.snapshot().records[&id].task.as_ref().unwrap().title,
        "Original"
    );
    a.save_task(Some(id.clone()), task("Device A")).unwrap();
    b.save_task(Some(id.clone()), task("Device B")).unwrap();
    client.sync(&mut a).await.unwrap();
    client.sync(&mut b).await.unwrap();
    assert!(b.snapshot().records[&id].conflict.is_some());
    assert_eq!(
        b.snapshot().records[&id].task.as_ref().unwrap().title,
        "Device B"
    );
    b.resolve(&id, true).unwrap();
    client.sync(&mut b).await.unwrap();
    client.sync(&mut a).await.unwrap();
    assert_eq!(
        a.snapshot().records[&id].task.as_ref().unwrap().title,
        "Device B"
    );
    b.delete_task(&id).unwrap();
    client.sync(&mut b).await.unwrap();
    client.sync(&mut a).await.unwrap();
    assert!(a.snapshot().records[&id].task.is_none());

    // An auth failure must not discard offline edits or advance the cursor.
    let queued = a.save_task(None, task("Queued offline")).unwrap();
    let before = a.snapshot().cursor;
    let invalid = SyncClient::new(&endpoint, format!("as_{}", "z".repeat(43))).unwrap();
    assert!(invalid.sync(&mut a).await.is_err());
    assert!(a.snapshot().records[&queued].dirty);
    assert_eq!(a.snapshot().cursor, before);
    client.sync(&mut a).await.unwrap();
    assert!(!a.snapshot().records[&queued].dirty);

    // Replay from cursor zero across more than one real server page.
    for n in 0..103 {
        a.save_task(None, task(&format!("Page item {n}"))).unwrap();
    }
    client.sync(&mut a).await.unwrap();
    let mut c = Store::open(":memory:").unwrap();
    client.sync(&mut c).await.unwrap();
    assert_eq!(c.snapshot().records.len(), 105);
    let cursor = c.snapshot().cursor;
    client.sync(&mut c).await.unwrap();
    assert_eq!(c.snapshot().cursor, cursor);
    assert!(
        c.snapshot()
            .records
            .values()
            .all(|r| !r.dirty && r.conflict.is_none())
    );

    let mut unbound = Store::open(":memory:").unwrap();
    let pending = unbound.save_task(None, task("Unauthorized")).unwrap();
    assert!(invalid.sync(&mut unbound).await.is_err());
    assert_eq!(unbound.snapshot().cursor, 0);
    assert!(unbound.snapshot().binding.is_none());
    assert!(unbound.snapshot().records[&pending].dirty);
}
