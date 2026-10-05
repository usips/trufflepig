use super::*;
use crate::board::board_backend::BoardBackend;
use crate::board::board_protocol::{
    BoardOp, BoardRequest, CommitCoauthor, CommitPlanLink, LinkedCommit,
};
use crate::identity::GitOid;

#[test]
fn collection_entries_page_shared_sequence_commits_without_skips() {
    let (_directory, mut board) = database();
    let root = GitOid::parse("1111111111111111111111111111111111111111").unwrap();
    let repo_key = RepoKey::from_roots([root]).unwrap();
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "s1",
    )
    .unwrap();
    let commits = (0..120u32)
        .map(|number| LinkedCommit {
            repo_key: repo_key.clone(),
            oid: GitOid::parse(&format!("{number:040x}")).unwrap(),
            subject: "implement task".to_owned(),
            committed_at: 50,
            author: "Josh".to_owned(),
            coauthors: vec![CommitCoauthor {
                harness: HarnessLabel::parse("codex").unwrap(),
                model: "Model".to_owned(),
                email: "noreply@openai.com".to_owned(),
            }],
            files: 1,
            insertions: 2,
            deletions: 1,
            plans: vec![CommitPlanLink {
                plan_id: plan(1),
                task_ordinal: None,
            }],
        })
        .collect();
    let reply = board
        .handle(&BoardRequest::new(actor, BoardOp::LinkCommits { commits }))
        .unwrap();
    let BoardResult::CommitsLinked(linked) = reply.result else {
        panic!("links");
    };
    assert_eq!(linked.inserted, 120);
    assert!(linked.unknown_plans.is_empty());
    let (minimum, maximum, total): (i64, i64, i64) = board
        .conn
        .query_row(
            "SELECT min(seq),max(seq),count(*) FROM entries WHERE kind='commit'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((minimum, maximum, total), (3, 3, 120));
    let reader = board.reader.as_ref().expect("read connection");
    let mut before = None;
    let mut through = None;
    let mut seen = Vec::new();
    let mut pages = 0;
    loop {
        let BoardResult::Entries(page) = entries_page(
            reader,
            Some(plan(1)),
            Some(EntryKind::Commit),
            None,
            None,
            None,
            None,
            None,
            None,
            before,
            through,
            50,
        )
        .unwrap()
        .result
        else {
            panic!("entries");
        };
        assert_eq!(page.plan, Some(plan(1)));
        assert!(page.next_after.is_none());
        if through.is_none() {
            through = Some(page.through);
        }
        seen.extend(page.entries.iter().map(|entry| entry.id));
        before = page.next_before;
        pages += 1;
        if before.is_none() {
            break;
        }
    }
    assert_eq!((pages, seen.len()), (3, 120));
    let mut ordered = seen.clone();
    ordered.sort();
    ordered.dedup();
    assert_eq!(ordered.len(), 120);
    assert!(
        seen.windows(2).all(|pair| pair[0] > pair[1]),
        "newest-first within the shared sequence"
    );
}
