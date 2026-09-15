use super::*;

fn definition(name: &str, start: usize, end: usize) -> Definition {
    Definition {
        name: name.into(),
        kind: "function".into(),
        start,
        end,
        container: None,
    }
}

fn spans(source: &[u8], definitions: &[Definition]) -> Vec<(usize, usize, Option<usize>)> {
    partition_source(source, definitions)
        .into_iter()
        .map(|span| (span.start, span.end, span.definition))
        .collect()
}

#[test]
fn packed_declaration_leaves_multibyte_tail_intact() {
    let source = format!("{}érest", "a".repeat(REGION_BYTES - 1));
    let definitions = [definition("prefix", 0, REGION_BYTES - 1)];
    let regions = spans(source.as_bytes(), &definitions);
    assert!(
        regions
            .iter()
            .all(|&(start, end, _)| source.is_char_boundary(start) && source.is_char_boundary(end))
    );
    assert_eq!(regions[0].1, REGION_BYTES - 1);
    assert_eq!(regions.last().unwrap().1, source.len());
}

#[test]
fn malformed_definition_boundaries_cannot_split_utf8() {
    let source = "éé tail".as_bytes();
    let definitions = [definition("malformed", 1, 3)];
    let regions = spans(source, &definitions);
    assert!(
        regions
            .iter()
            .all(|&(start, end, _)| std::str::from_utf8(&source[start..end]).is_ok())
    );
    assert_eq!(regions.first().unwrap().0, 0);
    assert_eq!(regions.last().unwrap().1, source.len());
}

#[test]
fn fitting_parent_is_one_region_even_with_nested_definitions() {
    let source = b"parent { child() }\n";
    let definitions = [
        definition("parent", 0, source.len()),
        definition("child", 9, 16),
    ];

    assert_eq!(
        spans(source, &definitions),
        vec![(0, source.len(), Some(0))]
    );
}

#[test]
fn oversized_parent_uses_direct_children_and_packs_gaps() {
    let mut source = vec![b'a'; 9000];
    source[3000..5000].fill(b'c');
    let definitions = [
        definition("parent", 0, source.len()),
        definition("child", 3000, 5000),
    ];

    let regions = spans(&source, &definitions);
    assert!(
        regions
            .iter()
            .all(|&(start, end, _)| end > start && end - start <= REGION_BYTES)
    );
    assert!(regions.windows(2).all(|pair| pair[0].1 == pair[1].0));
    assert_eq!(regions.first().unwrap().0, 0);
    assert_eq!(regions.last().unwrap().1, source.len());
    assert!(
        regions
            .iter()
            .any(|&(start, end, owner)| { start <= 3000 && 5000 <= end && owner == Some(0) })
    );
}

#[test]
fn invalid_duplicate_and_crossing_spans_do_not_break_partition() {
    let source = b"0123456789abcdefghij";
    let definitions = [
        definition("outer", 0, 10),
        definition("duplicate", 0, 10),
        definition("crossing", 5, 15),
        definition("invalid", 10, source.len() + 1),
    ];

    let regions = spans(source, &definitions);
    assert_eq!(regions.first().unwrap().0, 0);
    assert_eq!(regions.last().unwrap().1, source.len());
    assert!(regions.windows(2).all(|pair| pair[0].1 == pair[1].0));
    assert!(
        regions
            .iter()
            .all(|&(start, end, _)| end - start <= REGION_BYTES)
    );
}

#[test]
fn oversized_chunks_break_at_newline_and_utf8_boundaries() {
    let mut source = Vec::new();
    source.extend(std::iter::repeat_n(b'x', REGION_BYTES - 1));
    source.extend_from_slice("é\nrest".as_bytes());
    let regions = spans(&source, &[]);

    assert!(regions.windows(2).all(|pair| pair[0].1 == pair[1].0));
    assert!(regions.iter().all(|&(start, end, _)| {
        end - start <= REGION_BYTES && (end == source.len() || is_utf8_boundary(&source, end))
    }));
    assert_eq!(regions[0].1, REGION_BYTES - 1);
}

#[test]
fn empty_source_has_no_regions() {
    assert!(partition_source(b"", &[]).is_empty());
}
