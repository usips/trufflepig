use crate::{
    cli,
    query_deadline::QueryDeadline,
    workspace::{is_local_query, local_execution_deadline},
};
use std::{os::unix::net::UnixStream, sync::Arc, time::Duration};

fn options(words: &[&str]) -> cli::Arguments {
    cli::parse(
        &words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
fn local_read_cap_keeps_forwarding_deadline_available() {
    let options = options(&["search", "sym:SharedThing"]);
    let incoming = QueryDeadline::after(Duration::from_secs(30));
    let local = local_execution_deadline(&options, &incoming);

    assert!(local.remaining() <= Duration::from_secs(20));
    assert!(incoming.remaining() > Duration::from_secs(28));

    let forwarding = QueryDeadline::after(Duration::from_secs(24));
    let local = local_execution_deadline(&options, &forwarding);
    assert!(local.remaining() <= Duration::from_secs(20));
    assert!(forwarding.remaining() > Duration::from_secs(23));
}

#[test]
fn local_cap_preserves_shorter_expiry_and_cancellation() {
    let options = options(&["show", "sym:SharedThing"]);
    let short = QueryDeadline::after(Duration::from_millis(500));
    let short_remaining = short.remaining();
    let capped = local_execution_deadline(&options, &short);
    assert!(capped.remaining() <= short_remaining);

    let (server, client) = UnixStream::pair().unwrap();
    let incoming = QueryDeadline::after(Duration::from_secs(30)).with_peer(Arc::new(server));
    let capped = local_execution_deadline(&options, &incoming);
    drop(client);
    assert!(capped.expired());
}

#[test]
fn maintenance_keeps_the_incoming_deadline() {
    let options = options(&["index"]);
    assert!(!is_local_query(&options));

    let incoming = QueryDeadline::after(Duration::from_secs(30));
    let maintenance = local_execution_deadline(&options, &incoming);
    assert!(maintenance.remaining() > Duration::from_secs(28));
    assert!(incoming.remaining() > Duration::from_secs(28));
}

#[test]
fn local_read_classifier_covers_owner_read_verbs_and_status_views() {
    for words in [
        &[][..],
        &["search"][..],
        &["refs"][..],
        &["map"][..],
        &["show"][..],
        &["more"][..],
        &["ctx"][..],
        &["status"][..],
        &["doctor"][..],
        &["hist"][..],
        &["hist-status"][..],
        &["since"][..],
        &["diff"][..],
        &["blame"][..],
        &["session"][..],
        &["audit"][..],
        &["ws"][..],
        &["semantic", "status"][..],
        &["semantic", "worker", "status"][..],
    ] {
        assert!(is_local_query(&options(words)), "{words:?}");
    }
    for words in [
        &["init"][..],
        &["index"][..],
        &["hist-index"][..],
        &["forget-logs"][..],
        &["semantic", "check"][..],
        &["semantic", "prepare"][..],
        &["semantic", "worker", "serve"][..],
    ] {
        assert!(!is_local_query(&options(words)), "{words:?}");
    }
}
