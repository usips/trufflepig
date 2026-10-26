use super::{
    PreparedWorkspace, append_current_references, direct_source_fallback, indexed_fixture,
    member_rows, replaced_member,
};
use crate::workspace::retrieval::{MemberCommand, search_member, search_with_policy_using};
use serde_json::Value;
use tp_semantic::SemanticSession;

#[test]
fn explicit_workspace_replacement_recovers_lexical_regex_map_and_refs() {
    let fixture = indexed_fixture();
    for words in [
        &["search", "SharedThing marker", "ws:all"][..],
        &["search", "re:SharedThing", "ws:all"][..],
        &["map", "lib.rs", "ws:all"][..],
        &["refs", "SharedThing", "ws:all"][..],
    ] {
        if words[0] == "refs" {
            append_current_references(&fixture);
        }
        let prepared = PreparedWorkspace::new(&fixture, words);
        let request = prepared.request();
        let mut session = SemanticSession::default();
        let output = search_with_policy_using(
            &request,
            &mut session,
            |_request, _command: &MemberCommand<'_>, member, _first, _map_miss| {
                if member.name() == "pack" {
                    Ok(replaced_member())
                } else {
                    search_member(_request, _command, member, _first, _map_miss)
                }
            },
            direct_source_fallback,
        )
        .unwrap();
        let page: Value = serde_json::from_str(&output).unwrap();

        assert_eq!(page["source"], "current_source", "{words:?}: {page}");
        assert_eq!(page["router_fallback"]["member"], "pack", "{page}");
        assert_eq!(page["router_fallback"]["coverage"], "complete", "{page}");
        assert_eq!(member_rows(&page), ["engine", "pack", "upstream"], "{page}");
        for member in ["engine", "pack", "upstream"] {
            assert!(
                page["hits"].as_array().unwrap().iter().any(|hit| {
                    hit["member"] == member
                        && hit["handle"]
                            .as_str()
                            .is_some_and(|handle| handle.starts_with("read:path:"))
                }),
                "missing current-source handle for {member}: {page}"
            );
        }
    }
}
