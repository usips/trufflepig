use super::*;
use crate::identity::GitOid;

#[test]
fn canonical_addresses_round_trip_and_reject_zero_and_padding() {
    for value in ["P7", "P7@12", "P7@10..", "P7@10..14", "P7.3", "E482"] {
        let reference = BoardRef::parse(value).unwrap();
        assert_eq!(reference.to_string(), value);
        assert_eq!(
            serde_json::from_str::<BoardRef>(&serde_json::to_string(&reference).unwrap()).unwrap(),
            reference
        );
    }
    for value in [
        "P0",
        "P07",
        "P7@0",
        "P7@01",
        "P7.0",
        "E00",
        "P7@14..10",
        "P7@1..0",
        "P9223372036854775808",
    ] {
        assert!(BoardRef::parse(value).is_err(), "accepted {value}");
    }
    assert!(serde_json::from_str::<EventSeq>("9223372036854775808").is_err());
    assert_eq!(EventSeq::parse("0").unwrap().get(), 0);
}

#[test]
fn extraction_ignores_short_hex_and_ids_inside_words() {
    let oid = "0123456789abcdef0123456789abcdef01234567";
    let refs = extract_refs(&format!(
        "(P7.3), P7@12, E482. P7@10..14 P7.3 deadbeef {oid} xP7 P07 _E4"
    ));
    assert_eq!(refs.len(), 5);
    assert!(refs.contains(&BoardRef::Commit(GitOid::parse(oid).unwrap())));
    assert!(!refs.contains(&BoardRef::Plan(PlanId::new(7).unwrap())));
    assert!(extract_refs("abcdef0123456789abcdef0123456789abcdef0123").is_empty());
    let uppercase_oid = "E".repeat(40);
    assert!(matches!(
        BoardRef::parse(&uppercase_oid).unwrap(),
        BoardRef::Commit(_)
    ));
}

#[test]
fn repository_roots_are_sorted_deduplicated_and_validated() {
    let first = GitOid::parse(&"a".repeat(40)).unwrap();
    let second = GitOid::parse(&"b".repeat(40)).unwrap();
    let key = RepoKey::from_roots([second, first, first]).unwrap();
    assert_eq!(key.to_string(), format!("{first},{second}"));
    assert_eq!(RepoKey::parse(key.as_str()).unwrap(), key);
    assert!(RepoKey::parse(&format!("{second},{first}")).is_err());
    assert!(RepoKey::from_roots([]).is_err());
}
