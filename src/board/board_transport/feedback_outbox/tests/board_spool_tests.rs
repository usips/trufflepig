use super::*;

#[test]
fn queue_durably_publishes_private_complete_records_without_a_database() {
    let dir = scratch();
    let spool = dir.path().join("absent-spool");
    let request = report();
    let reply = queue(&spool, &request).unwrap();
    let BoardResult::Queued { import_key } = reply.result else {
        panic!("expected queue acknowledgement");
    };
    let path = spool.join(format!("{import_key}.feedback"));
    assert_eq!(read_record(&path).unwrap(), request);
    assert_eq!(
        spool.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::read_dir(&spool).unwrap().count(), 1);
    queue(&spool, &request).unwrap();
    assert_eq!(fs::read_dir(&spool).unwrap().count(), 1);
}

#[test]
fn queued_uuid_cannot_be_replaced_by_different_feedback() {
    let dir = scratch();
    let mut request = report();
    queue(dir.path(), &request).unwrap();
    let BoardOp::Feedback { summary, .. } = &mut request.op else {
        unreachable!()
    };
    *summary = EntryText::new("different evidence").unwrap();
    assert_eq!(
        queue(dir.path(), &request).unwrap_err().code,
        BoardErrorCode::InvalidOptions
    );
    let mut backend = ImportBackend::default();
    assert_eq!(
        import_pending(dir.path(), &mut backend).unwrap().imported,
        1
    );
}

#[test]
fn symbolic_link_records_never_read_or_modify_their_targets() {
    let dir = scratch();
    let target = dir.path().join("outside-report");
    fs::write(&target, b"private evidence").unwrap();
    let modified = target.metadata().unwrap().modified().unwrap();
    let link = dir.path().join(format!("{}.feedback", new_import_key()));
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut backend = ImportBackend::default();
    assert_eq!(
        import_pending(dir.path(), &mut backend)
            .unwrap()
            .quarantined,
        1
    );
    assert_eq!(fs::read(&target).unwrap(), b"private evidence");
    assert_eq!(target.metadata().unwrap().modified().unwrap(), modified);
    assert!(backend.entries.is_empty());
}

#[test]
fn invalid_reports_and_unwritable_spool_never_get_queue_acknowledgements() {
    let dir = scratch();
    let path = dir.path().join("file-instead-of-directory");
    fs::write(&path, b"existing").unwrap();
    assert_eq!(
        queue(&path, &report()).unwrap_err().code,
        BoardErrorCode::BoardUnavailable
    );
    let mut request = report();
    request.op = BoardOp::FeedbackList {
        open_only: true,
        after: None,
        through: None,
        limit: 200,
    };
    assert_eq!(
        queue(dir.path(), &request).unwrap_err().code,
        BoardErrorCode::InvalidOptions
    );
}

#[test]
fn queue_makes_existing_permissive_spool_private_before_publishing() {
    let directory = scratch();
    let spool = directory.path().join("existing-spool");
    fs::create_dir(&spool).unwrap();
    fs::set_permissions(&spool, fs::Permissions::from_mode(0o755)).unwrap();
    let reply = queue(&spool, &report()).unwrap();
    let BoardResult::Queued { import_key } = reply.result else {
        panic!("expected durable queue acknowledgement");
    };
    assert_eq!(
        spool.metadata().unwrap().permissions().mode() & 0o7777,
        0o700
    );
    assert_eq!(
        spool
            .join(format!("{import_key}.feedback"))
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
}

#[test]
fn spool_symlink_and_ownership_rejections_preserve_target_permissions() {
    let directory = scratch();
    let target = directory.path().join("outside-spool");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    let spool = directory.path().join("spool-link");
    std::os::unix::fs::symlink(&target, &spool).unwrap();
    assert_eq!(
        queue(&spool, &report()).unwrap_err().code,
        BoardErrorCode::InvalidOptions
    );
    assert_eq!(
        target.metadata().unwrap().permissions().mode() & 0o7777,
        0o755
    );
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    let actual_owner = target.metadata().unwrap().uid();
    assert_eq!(
        private_directory_owned_by(&target, actual_owner.wrapping_add(1))
            .unwrap_err()
            .code,
        BoardErrorCode::InvalidOptions
    );
    assert_eq!(
        target.metadata().unwrap().permissions().mode() & 0o7777,
        0o755
    );
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
}

#[test]
fn newly_quarantined_old_records_receive_a_full_retention_window() {
    let directory = scratch();
    let path = directory
        .path()
        .join(format!("{}.feedback", new_import_key()));
    fs::write(&path, b"old malformed record").unwrap();
    let file = File::open(&path).unwrap();
    file.set_times(
        fs::FileTimes::new()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)),
    )
    .unwrap();
    let started = std::time::SystemTime::now();
    import_pending(directory.path(), &mut ImportBackend::default()).unwrap();
    let quarantined = fs::read_dir(directory.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert!(quarantined.metadata().unwrap().modified().unwrap() >= started);
}
