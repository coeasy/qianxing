use super::*;

#[cfg(test)]
#[test]
fn atomic_file_write_faults_before_replace_keep_old_file_and_remove_temp() {
    for stage in [
        AtomicWriteStage::TempWritten,
        AtomicWriteStage::FlushStepReturned,
    ] {
        let root = crate::test_temp_dir().join(format!(
            "qx-atomic-precommit-{}-{}",
            std::process::id(),
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let path = root.join("state.json");
        write_atomic_path(&path, &root, "committed").unwrap();

        let error =
            write_atomic_path_with_hook(&path, &root, "uncommitted", sync_file, |current| {
                if current == stage {
                    Err(StorageError::Io(format!("injected after {current:?}")))
                } else {
                    Ok(())
                }
            })
            .expect_err("injected pre-commit fault must be reported");

        assert!(matches!(error, StorageError::Io(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "committed");
        assert_eq!(
            std::fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
                .count(),
            0,
            "failed pre-commit writes must clean their temporary file"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn atomic_file_write_flush_failure_preserves_destination_and_cleans_temp() {
    let root = crate::test_temp_dir().join(format!(
        "qx-atomic-flush-failure-{}-{}",
        std::process::id(),
        TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let path = root.join("state.json");
    write_atomic_path(&path, &root, "committed").unwrap();

    let error = write_atomic_path_with_hook(
        &path,
        &root,
        "uncommitted",
        |_| Err(StorageError::Io("injected flush failure".into())),
        |_| Ok(()),
    )
    .expect_err("flush failure must stop before replacing the destination");

    assert!(matches!(error, StorageError::Io(_)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "committed");
    assert_eq!(
        std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .count(),
        0
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn atomic_file_write_reports_post_replace_fault_with_new_file_visible() {
    let root = crate::test_temp_dir().join(format!(
        "qx-atomic-postreplace-{}-{}",
        std::process::id(),
        TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let path = root.join("state.json");
    write_atomic_path(&path, &root, "committed").unwrap();

    let error = write_atomic_path_with_hook(&path, &root, "newly-committed", sync_file, |stage| {
        if stage == AtomicWriteStage::Replaced {
            Err(StorageError::Io("injected after atomic replace".into()))
        } else {
            Ok(())
        }
    })
    .expect_err("post-replace fault must report uncertain completion");

    assert!(matches!(error, StorageError::Io(_)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "newly-committed");
    assert_eq!(
        std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .count(),
        0
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn atomic_file_write_rename_failure_preserves_existing_destination() {
    let root = crate::test_temp_dir().join(format!(
        "qx-atomic-rename-failure-{}-{}",
        std::process::id(),
        TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(root.join("state.json")).unwrap();
    std::fs::write(root.join("state.json/child"), "existing").unwrap();

    let error = write_atomic_path(&root.join("state.json"), &root, "replacement")
        .expect_err("a file must not replace a directory");

    assert!(matches!(error, StorageError::Io(_)));
    assert_eq!(
        std::fs::read_to_string(root.join("state.json/child")).unwrap(),
        "existing"
    );
    assert_eq!(
        std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .count(),
        0
    );
    std::fs::remove_dir_all(root).unwrap();
}
