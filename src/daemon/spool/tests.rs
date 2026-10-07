use super::*;
use std::fs::File;

#[test]
fn request_publication_does_not_rename_after_temp_write_expires_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let request_path = dir.path().join("request.request");
    let temporary = temporary_path(&request_path);
    let deadline = QueryDeadline::after(Duration::from_millis(250));
    let mut wrote_temporary = false;

    let error = write_request_atomic(&request_path, b"request", &deadline, |temporary, bytes| {
        fs::write(temporary, bytes)?;
        wrote_temporary = true;
        while !deadline.expired() {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    })
    .unwrap_err();

    assert!(wrote_temporary, "test did not reach the temporary write");
    assert!(deadline::is_timed_out(&error), "{error:#}");
    assert!(!request_path.exists(), "expired request was published");
    assert!(
        !temporary.exists(),
        "expired temporary request was retained"
    );
}

#[test]
fn router_restart_and_orphan_sweep_preserve_durable_feedback() {
    let dir = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4();
    let durable = ["feedback", "feedback.quarantine"];
    let modified = SystemTime::now() - ORPHAN_AGE - Duration::from_secs(1);
    for extension in durable {
        let path = dir.path().join(format!("{id}.{extension}"));
        fs::write(&path, b"durable report").unwrap();
        File::open(&path).unwrap().set_modified(modified).unwrap();
    }
    let transient = dir.path().join(format!("{id}.reply"));
    fs::write(&transient, b"reply").unwrap();
    let mut server = SpoolServer::open(dir.path()).unwrap();
    assert!(!transient.exists());
    assert!(server.claim().is_empty());
    for extension in durable {
        assert!(dir.path().join(format!("{id}.{extension}")).exists());
    }
}

#[test]
fn restart_reaps_abandoned_pending_and_expires_quarantine_after_thirty_days() {
    let dir = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4();
    let cases = [
        (
            format!("{id}.feedback"),
            QUARANTINE_AGE + Duration::from_secs(1),
            true,
        ),
        (
            format!("{id}.feedback.pending"),
            ORPHAN_AGE + Duration::from_secs(1),
            false,
        ),
        (
            format!("{id}.feedback.quarantine"),
            ORPHAN_AGE + Duration::from_secs(1),
            true,
        ),
        (
            format!("{id}.feedback.old.quarantine"),
            QUARANTINE_AGE + Duration::from_secs(1),
            false,
        ),
        (
            "unknown.old".to_owned(),
            ORPHAN_AGE + Duration::from_secs(1),
            false,
        ),
        ("unknown.fresh".to_owned(), Duration::ZERO, true),
        (format!("{id}.feedback.fresh.pending"), Duration::ZERO, true),
    ];
    for (name, age, _) in &cases {
        let path = dir.path().join(name);
        fs::write(&path, b"fixture").unwrap();
        File::open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - *age)
            .unwrap();
    }
    let mut server = SpoolServer::open(dir.path()).unwrap();
    assert!(server.claim().is_empty());
    for (name, _, retained) in cases {
        assert_eq!(dir.path().join(&name).exists(), retained, "{name}");
    }
}

#[test]
fn orphan_sweep_removes_only_expired_transport_files() {
    let dir = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4();
    let old = dir.path().join(format!("{id}.reply.tmp"));
    let fresh = dir.path().join(format!("{id}.reply"));
    fs::write(&old, b"old reply").unwrap();
    fs::write(&fresh, b"fresh reply").unwrap();
    File::open(&old)
        .unwrap()
        .set_modified(SystemTime::now() - ORPHAN_AGE - Duration::from_secs(1))
        .unwrap();
    remove_orphan(&old);
    remove_orphan(&fresh);
    assert!(!old.exists());
    assert!(fresh.exists());
}
