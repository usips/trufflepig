use super::*;

pub(super) fn compare_trees(
    old: BTreeMap<String, TreeFile>,
    mut new: BTreeMap<String, TreeFile>,
) -> Result<Vec<FileChange>> {
    let mut auxiliary = StagingBudget::new(staging::AUXILIARY_BYTES);
    auxiliary.reserve(64 * 1024)?;
    let auxiliary_per_file = 2 * std::mem::size_of::<TreeFile>() + 256;
    let mut deleted = Vec::with_capacity(old.len().min(128));
    let mut added = Vec::with_capacity(new.len().min(128));
    let mut changes = Vec::with_capacity(old.len().min(128));
    let mut budget = StagingBudget::new(staging::CHANGE_BYTES);
    for (path, file) in old {
        match new.remove(&path) {
            Some(next) if next.oid == file.oid && next.mode == file.mode => {}
            Some(next) => push_change(
                &mut changes,
                &mut budget,
                FileChange {
                    before: Some(file),
                    after: Some(next),
                    status: "modified".into(),
                },
            )?,
            None => {
                auxiliary.reserve(auxiliary_per_file)?;
                deleted.push(file);
            }
        }
    }
    auxiliary.reserve(
        new.len()
            .checked_mul(auxiliary_per_file)
            .context("history_resource_limited: comparison entry count overflow")?,
    )?;
    added.extend(new.into_values().map(Some));
    let mut old_counts = HashMap::with_capacity(deleted.len());
    let mut new_indexes = HashMap::with_capacity(added.len());
    for file in &deleted {
        *old_counts.entry(file.oid).or_insert(0usize) += 1;
    }
    for (index, file) in added.iter().enumerate() {
        let file = file.as_ref().expect("unclaimed addition");
        let item = new_indexes.entry(file.oid).or_insert((0usize, index));
        item.0 += 1;
    }
    for file in deleted {
        let rename = new_indexes
            .get(&file.oid)
            .filter(|&&(count, index)| {
                count == 1
                    && old_counts[&file.oid] == 1
                    && added[index].as_ref().is_some_and(|a| a.mode == file.mode)
            })
            .map(|&(_, index)| index);
        let change = if let Some(index) = rename {
            FileChange {
                before: Some(file),
                after: added[index].take(),
                status: "renamed_exact_blob".into(),
            }
        } else {
            FileChange {
                before: Some(file),
                after: None,
                status: "deleted".into(),
            }
        };
        push_change(&mut changes, &mut budget, change)?;
    }
    for file in added.into_iter().flatten() {
        push_change(
            &mut changes,
            &mut budget,
            FileChange {
                before: None,
                after: Some(file),
                status: "added".into(),
            },
        )?;
    }
    changes.sort_by(|a, b| {
        a.after
            .as_ref()
            .or(a.before.as_ref())
            .map(|f| &f.path)
            .cmp(&b.after.as_ref().or(b.before.as_ref()).map(|f| &f.path))
    });
    Ok(changes)
}

fn change_charge(change: &FileChange) -> Result<usize> {
    let mut size = 2 * std::mem::size_of::<FileChange>() + change.status.capacity();
    for file in change.before.iter().chain(change.after.iter()) {
        ensure!(
            file.path.len() <= staging::PATH_BYTES,
            "history_resource_limited: cached path staging limit"
        );
        size += file.path.capacity() + file.mode.capacity();
    }
    Ok(size)
}

fn push_change(
    changes: &mut Vec<FileChange>,
    budget: &mut StagingBudget,
    change: FileChange,
) -> Result<()> {
    budget.reserve(change_charge(&change)?)?;
    changes.push(change);
    Ok(())
}

pub(super) fn decode_changes(payload: &str) -> Result<Vec<FileChange>> {
    use serde::de::{Error, SeqAccess, Visitor};
    struct Changes;
    impl<'de> Visitor<'de> for Changes {
        type Value = Vec<FileChange>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded historical changes")
        }
        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut changes = Vec::with_capacity(128);
            let mut budget = StagingBudget::new(staging::CHANGE_BYTES);
            while let Some(change) = seq.next_element::<FileChange>()? {
                push_change(&mut changes, &mut budget, change).map_err(A::Error::custom)?;
            }
            Ok(changes)
        }
    }
    ensure!(
        payload.len() <= staging::CACHE_BYTES,
        "history_resource_limited: cached payload staging limit"
    );
    let mut decoder = serde_json::Deserializer::from_str(payload);
    let changes = serde::de::Deserializer::deserialize_seq(&mut decoder, Changes)?;
    decoder.end()?;
    Ok(changes)
}
