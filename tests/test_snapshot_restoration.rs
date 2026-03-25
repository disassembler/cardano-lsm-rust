// Test snapshot save and restore functionality
//
// Verifies that:
// 1. Snapshots can be saved
// 2. LSM trees can be opened from snapshots
// 3. Data is correctly restored
// 4. Snapshot SSTable files survive open_snapshot + drop (regression for destruction bug)

use cardano_lsm::{LsmTree, LsmConfig, Key, Value};
use tempfile::TempDir;

#[test]
fn test_snapshot_save_and_restore() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path();

    // Create tree and insert data
    {
        let mut tree = LsmTree::open(db_path, LsmConfig::default()).unwrap();

        tree.insert(&Key::from(b"key1"), &Value::from(b"value1")).unwrap();
        tree.insert(&Key::from(b"key2"), &Value::from(b"value2")).unwrap();
        tree.insert(&Key::from(b"key3"), &Value::from(b"value3")).unwrap();

        // Save snapshot
        tree.save_snapshot("test_snap", "Test snapshot").unwrap();
    }
    // Tree is closed here (dropped)

    // Open from snapshot
    let tree = LsmTree::open_snapshot(db_path, "test_snap").unwrap();

    // Verify data is restored
    assert_eq!(
        tree.get(&Key::from(b"key1")).unwrap(),
        Some(Value::from(b"value1"))
    );
    assert_eq!(
        tree.get(&Key::from(b"key2")).unwrap(),
        Some(Value::from(b"value2"))
    );
    assert_eq!(
        tree.get(&Key::from(b"key3")).unwrap(),
        Some(Value::from(b"value3"))
    );
}

#[test]
fn test_snapshot_restore_with_more_writes() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path();

    // Create tree, insert data, save snapshot
    {
        let mut tree = LsmTree::open(db_path, LsmConfig::default()).unwrap();

        tree.insert(&Key::from(b"key1"), &Value::from(b"value1")).unwrap();
        tree.insert(&Key::from(b"key2"), &Value::from(b"value2")).unwrap();

        tree.save_snapshot("snap1", "First snapshot").unwrap();
    }

    // Open from snapshot and add more data
    {
        let mut tree = LsmTree::open_snapshot(db_path, "snap1").unwrap();

        // Verify original data
        assert_eq!(
            tree.get(&Key::from(b"key1")).unwrap(),
            Some(Value::from(b"value1"))
        );

        // Add new data
        tree.insert(&Key::from(b"key3"), &Value::from(b"value3")).unwrap();
        tree.insert(&Key::from(b"key4"), &Value::from(b"value4")).unwrap();

        // Save another snapshot
        tree.save_snapshot("snap2", "Second snapshot").unwrap();
    }

    // Open from second snapshot
    let tree = LsmTree::open_snapshot(db_path, "snap2").unwrap();

    // Verify all data is there
    assert_eq!(
        tree.get(&Key::from(b"key1")).unwrap(),
        Some(Value::from(b"value1"))
    );
    assert_eq!(
        tree.get(&Key::from(b"key2")).unwrap(),
        Some(Value::from(b"value2"))
    );
    assert_eq!(
        tree.get(&Key::from(b"key3")).unwrap(),
        Some(Value::from(b"value3"))
    );
    assert_eq!(
        tree.get(&Key::from(b"key4")).unwrap(),
        Some(Value::from(b"value4"))
    );
}

#[test]
fn test_snapshot_restore_nonexistent() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path();

    // Try to open from non-existent snapshot
    let result = LsmTree::open_snapshot(db_path, "nonexistent");

    assert!(result.is_err());
    match result {
        Err(e) => assert!(e.to_string().contains("does not exist")),
        Ok(_) => panic!("Expected error, got Ok"),
    }
}

#[test]
fn test_snapshot_list_after_restore() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path();

    // Create and save snapshots
    {
        let mut tree = LsmTree::open(db_path, LsmConfig::default()).unwrap();
        tree.insert(&Key::from(b"key1"), &Value::from(b"value1")).unwrap();
        tree.save_snapshot("snap1", "First").unwrap();
        tree.save_snapshot("snap2", "Second").unwrap();
    }

    // Open from snapshot and list snapshots
    let tree = LsmTree::open_snapshot(db_path, "snap1").unwrap();
    let snapshots = tree.list_snapshots().unwrap();

    // Should see both snapshots
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.contains(&"snap1".to_string()));
    assert!(snapshots.contains(&"snap2".to_string()));
}

/// Regression test: snapshot SSTable files must survive open_snapshot + drop.
///
/// Before the fix, open_snapshot opened SsTableHandle objects whose paths pointed
/// directly at the snapshot directory. SsTableHandle::Drop calls delete_files() when
/// the refcount reaches zero, so dropping the tree deleted the snapshot's SSTable files,
/// leaving only metadata + metadata.checksum. The snapshot was silently destroyed and
/// could never be restored again.
///
/// After the fix, open_snapshot hard-links snapshot files into active/ first and opens
/// handles from active/. The snapshot directory is never touched by Drop.
#[test]
fn test_snapshot_files_survive_open_and_drop() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path();

    // Create tree, insert enough data to flush to SSTables, save snapshot.
    {
        let mut tree = LsmTree::open(db_path, LsmConfig::default()).unwrap();
        for i in 0u32..100 {
            let key = Key::from(format!("key{:04}", i).as_bytes());
            let val = Value::from(format!("value{:04}", i).as_bytes());
            tree.insert(&key, &val).unwrap();
        }
        tree.save_snapshot("snap1", "regression test").unwrap();
    }

    let snap_dir = db_path.join("snapshots").join("snap1");

    // Count SSTable data files (not metadata) before restore.
    let sstable_files_before: Vec<_> = std::fs::read_dir(&snap_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .filter(|n| {
            let s = n.to_string_lossy();
            s.ends_with(".keyops") || s.ends_with(".blobs")
                || s.ends_with(".filter") || s.ends_with(".index")
                || s.ends_with(".checksums")
        })
        .collect();

    // Skip the test body if the tree was small enough to stay entirely in memtable
    // (no SSTables flushed yet). The bug only manifests when there are SSTable files.
    if sstable_files_before.is_empty() {
        // Force a flush by opening the snapshot tree (which may flush memtable on save),
        // then re-check.
        let mut tree = LsmTree::open_snapshot(db_path, "snap1").unwrap();
        for i in 100u32..200 {
            let key = Key::from(format!("key{:04}", i).as_bytes());
            let val = Value::from(format!("value{:04}", i).as_bytes());
            tree.insert(&key, &val).unwrap();
        }
        tree.save_snapshot("snap2", "with sstables").unwrap();
        drop(tree);

        let snap2_dir = db_path.join("snapshots").join("snap2");
        let files: Vec<_> = std::fs::read_dir(&snap2_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .filter(|n| {
                let s = n.to_string_lossy();
                s.ends_with(".keyops") || s.ends_with(".blobs")
                    || s.ends_with(".filter") || s.ends_with(".index")
                    || s.ends_with(".checksums")
            })
            .collect();

        if files.is_empty() {
            return; // Still no SSTables; nothing to test.
        }

        // Now verify snap2 files survive open + drop.
        let _ = LsmTree::open_snapshot(db_path, "snap2").unwrap();
        // Drop happens here.

        for name in &files {
            let path = snap2_dir.join(name);
            assert!(
                path.exists(),
                "SSTable file {:?} was deleted by open_snapshot + drop (regression)",
                path
            );
        }
        return;
    }

    // Normal path: snap1 already has SSTable files.
    // Open from snapshot and immediately drop.
    let _ = LsmTree::open_snapshot(db_path, "snap1").unwrap();
    // Drop happens here.

    // Every SSTable file that existed before must still exist.
    for name in &sstable_files_before {
        let path = snap_dir.join(name);
        assert!(
            path.exists(),
            "SSTable file {:?} was deleted by open_snapshot + drop (regression)",
            path
        );
    }

    // Verify we can still open and read from the snapshot a second time.
    let tree = LsmTree::open_snapshot(db_path, "snap1").unwrap();
    assert_eq!(
        tree.get(&Key::from(b"key0000")).unwrap(),
        Some(Value::from(b"value0000"))
    );
}
