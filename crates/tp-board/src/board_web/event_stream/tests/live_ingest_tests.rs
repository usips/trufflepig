use super::*;

#[test]
fn live_stream_response_carries_security_headers() {
    let streams = streams(empty_reader());
    let mut client = spawn(&streams, 0);
    let headers = read_until(&mut client, "\r\n\r\n");
    assert!(
        headers.contains(concat!(
            "Content-Security-Policy: default-src 'self'; frame-ancestors 'none'; ",
            "base-uri 'none'; object-src 'none'; form-action 'none'\r\n"
        )),
        "{headers}"
    );
    assert!(
        headers.contains("Referrer-Policy: no-referrer\r\n"),
        "{headers}"
    );
    assert!(
        headers.contains("X-Content-Type-Options: nosniff\r\n"),
        "{headers}"
    );
    assert!(headers.contains("Cache-Control: no-store\r\n"), "{headers}");
    drop(client);
    wait_released(&streams);
}

#[test]
fn ingest_results_reach_live_subscribers_without_an_event_cursor() {
    let streams = streams(empty_reader());
    let mut client = spawn(&streams, 0);
    read_until(&mut client, "\r\n\r\n");
    streams.publish_ingest(&serde_json::json!({"inserted": 3}));
    let frame = read_until(&mut client, "event: ingest\n");
    assert!(frame.contains("data: {\"inserted\":3}\n\n"), "{frame}");
    assert!(!frame.contains("\nid:"), "{frame}");
    drop(client);
    wait_released(&streams);
}

#[test]
fn ingest_results_before_subscription_replay_for_ticket_filtering() {
    let streams = streams(empty_reader());
    streams.publish_ingest(&serde_json::json!({"inserted": 1}));
    let mut client = spawn(&streams, 0);
    let replay = read_until(&mut client, "\"inserted\":1");
    assert!(replay.contains("event: ingest\n"), "{replay}");
    drop(client);
    wait_released(&streams);
}

#[test]
fn resubscribed_stream_receives_receipts_published_while_away() {
    let streams = streams(empty_reader());
    let mut client = spawn(&streams, 0);
    read_until(&mut client, "\r\n\r\n");
    drop(client);
    wait_released(&streams);
    streams.publish_ingest(&serde_json::json!({"inserted": 7}));
    let mut resubscribed = spawn(&streams, 0);
    let replay = read_until(&mut resubscribed, "\"inserted\":7");
    assert!(replay.contains("event: ingest\n"), "{replay}");
    drop(resubscribed);
    wait_released(&streams);
}

#[test]
fn resubscribe_replays_only_the_last_eight_receipts() {
    let streams = streams(empty_reader());
    for n in 1..=10 {
        streams.publish_ingest(&serde_json::json!({"n": n}));
    }
    let mut client = spawn(&streams, 0);
    let replay = read_until(&mut client, "\"n\":10");
    for n in 3..=10 {
        assert!(replay.contains(&format!("\"n\":{n}")), "{replay}");
    }
    assert!(!replay.contains("\"n\":1}"), "{replay}");
    assert!(!replay.contains("\"n\":2}"), "{replay}");
    drop(client);
    wait_released(&streams);
}
