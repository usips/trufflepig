use super::*;

#[test]
fn inbox_reminder_query_caps_materialization_and_reports_omissions() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    for index in 0..30 {
        post(
            &mut board,
            "claude",
            plan,
            EntryKind::Question,
            &format!("question {index}"),
            None,
        );
    }
    let bounded = scoped_feed(&mut board, None, true, 2);
    assert_eq!(bounded.open.len(), 2);
    assert_eq!(bounded.open_omitted, 28);
    let capped = scoped_feed(&mut board, None, true, 100);
    assert_eq!(capped.open.len(), 20);
    assert_eq!(capped.open_omitted, 10);
}

#[test]
fn inbox_reminder_count_is_bounded_and_reports_capped_omissions() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    for index in 0..210 {
        post(
            &mut board,
            "claude",
            plan,
            EntryKind::Question,
            &format!("reminder {index}"),
            None,
        );
    }
    let inbox = scoped_feed(&mut board, None, true, 100);
    assert_eq!(inbox.open.len(), 20);
    assert_eq!(inbox.open[0].body.as_str(), "reminder 0");
    assert_eq!(
        inbox.open_omitted, 180,
        "the reminder count stops at its cap instead of scanning every entry"
    );
}

#[test]
fn inbox_reminder_count_flags_lower_bound_only_at_the_cap() {
    let (_directory, mut board) = database();
    let crowded = plan(&mut board);
    for index in 0..210 {
        post(
            &mut board,
            "claude",
            crowded,
            EntryKind::Question,
            &format!("reminder {index}"),
            None,
        );
    }
    let capped = scoped_feed(&mut board, None, true, 100);
    assert_eq!(capped.open.len(), 20);
    assert_eq!(capped.open_omitted, 180);
    assert!(
        capped.open_omitted_lower_bound,
        "a count that hits the cap is a lower bound"
    );
    let value = serde_json::to_value(&capped).unwrap();
    assert_eq!(
        value["open_omitted_lower_bound"],
        serde_json::Value::Bool(true)
    );
    let (_directory, mut board) = database();
    let small = plan(&mut board);
    post(
        &mut board,
        "claude",
        small,
        EntryKind::Question,
        "lone reminder",
        None,
    );
    let exact = scoped_feed(&mut board, None, true, 100);
    assert_eq!(exact.open_omitted, 0);
    assert!(
        !exact.open_omitted_lower_bound,
        "a count below the cap is exact"
    );
    let value = serde_json::to_value(&exact).unwrap();
    assert_eq!(
        value["open_omitted_lower_bound"],
        serde_json::Value::Bool(false)
    );
}
