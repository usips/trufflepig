use super::*;
use anyhow::{Context, ensure};
use serde_json::json;

pub(super) fn blame(
    history: &History,
    store: &mut Store,
    target: &str,
    raw: bool,
    ignore_revs_file: Option<&Path>,
    budget: &OutputBudget,
) -> Result<String> {
    let target = match commands::select(store, Some(target), budget)? {
        Ok(target) => target.context("missing target")?,
        Err(response) => return Ok(response),
    };
    let decoded = crate::store::decode_path(&target.path)?;
    let path = history
        .repository
        .repository_path(decoded.to_str().context("blame path is not UTF-8")?)?;
    let (bytes, blob, revision, publication, endpoint) = if let Some(commit) = &target.commit {
        let tree = comparison::tree(history, Some(commit))?;
        let file = tree
            .get(&target.path)
            .context("history_unavailable: path is absent at selected commit")?;
        let source = comparison::source(history, commit, file)?;
        (
            history.repository.blob(&source.blob)?,
            Some(source.blob),
            None,
            None,
            "historical_commit",
        )
    } else {
        ensure_captured_head(history)?;
        let tree = comparison::tree(history, Some(&history.tip))?;
        let file = tree
            .get(&target.path)
            .context("history_unavailable: untracked path has no captured HEAD ancestry")?;
        ensure!(
            matches!(file.mode.as_str(), "100644" | "100755"),
            "history_unavailable: blame requires a regular file"
        );
        let (bytes, revision, publication) = published_source(store, &target)?;
        (
            bytes,
            None,
            Some(revision),
            Some(publication),
            "published_working_tree",
        )
    };
    let span = target
        .span
        .unwrap_or(crate::identity::ByteSpan::new(0, bytes.len())?)
        .validate(bytes.len())?;
    let (first, last) = crate::source::line_span(&bytes, span.start, span.end);
    let range = format!("{first},{last}");
    let mut args = vec![
        "blame",
        "--first-parent",
        "--line-porcelain",
        "-L",
        range.as_str(),
    ];
    if !raw {
        args.extend(["-M", "-w"]);
    }
    let ignored = ignore_revs_file.map(|p| p.canonicalize()).transpose()?;
    if let Some(path) = &ignored {
        ensure!(
            path.is_file() && path.metadata()?.len() <= MAX_BLOB_BYTES as u64,
            "history_resource_limited: invalid ignore revisions file"
        );
        args.extend([
            "--ignore-revs-file",
            path.to_str()
                .context("ignore revisions file requires UTF-8")?,
        ]);
    }
    if let Some(commit) = &target.commit {
        args.push(commit.as_str());
    } else {
        args.extend(["--contents", "-"]);
    }
    args.extend(["--", path.as_str()]);
    let output = if span.start == span.end {
        Vec::new()
    } else if target.commit.is_some() {
        history.repository.run(&args)?
    } else {
        history.repository.run_with_input(&args, &bytes)?
    };
    if target.commit.is_none() {
        ensure_captured_head(history)?;
    }
    let mut runs: Vec<Value> = Vec::with_capacity(128);
    let mut retained_runs = staging::StagingBudget::new(staging::HUNK_BYTES);
    let mut pending = None;
    let mut ignored_line = false;
    let mut unblamable = false;
    let count = if span.start == span.end {
        0
    } else {
        last - first + 1
    };
    let mut expected_lines = bytes
        .split_inclusive(|&byte| byte == b'\n')
        .enumerate()
        .skip(first - 1)
        .take(count);
    for line in output.split(|&b| b == b'\n') {
        if line.first() == Some(&b'\t') {
            let (oid, original, final_line) = pending
                .take()
                .context("history_unavailable: incomplete blame record")?;
            let (index, expected) = expected_lines
                .next()
                .context("history_unavailable: unexpected blame source line")?;
            let expected = expected.strip_suffix(b"\n").unwrap_or(expected);
            ensure!(
                line[1..] == *expected && final_line == index as u64 + 1,
                "history_unavailable: Git attributes transformed supplied source bytes"
            );
            let can_join = runs.last().is_some_and(|r| {
                r["commit"] == json!(oid)
                    && r["original_start"].as_u64().unwrap_or(0) + r["lines"].as_u64().unwrap_or(0)
                        == original
                    && r["start_line"].as_u64().unwrap_or(0) + r["lines"].as_u64().unwrap_or(0)
                        == final_line
                    && r["ignored"] == json!(ignored_line)
                    && r["unblamable"] == json!(unblamable)
            });
            if can_join {
                let last = runs.last_mut().expect("contiguous run");
                last["lines"] = json!(last["lines"].as_u64().unwrap_or(0) + 1);
            } else {
                // Reserve the run, JSON container clone, and serialized response together.
                retained_runs.reserve(3 * 1024)?;
                runs.push(json!({"commit":oid,"original_start":original,"start_line":final_line,"lines":1,"ignored":ignored_line,"unblamable":unblamable}));
            }
            ignored_line = false;
            unblamable = false;
        } else if line == b"ignored" {
            ignored_line = true;
        } else if line == b"unblamable" {
            unblamable = true;
        } else if let Ok(text) = std::str::from_utf8(line) {
            let fields = text.split_whitespace().collect::<Vec<_>>();
            if fields.len() >= 3 && GitOid::parse(fields[0]).is_ok() {
                pending = Some((
                    fields[0].to_owned(),
                    fields[1].parse::<u64>()?,
                    fields[2].parse::<u64>()?,
                ));
            }
        }
    }
    ensure!(
        expected_lines.next().is_none() && pending.is_none(),
        "history_unavailable: incomplete blame source coverage"
    );
    let total = runs.len();
    loop {
        let value = json!({"operation":"blame","tip":history.tip,"path":target.path,"commit":target.commit,"blob":blob,"revision":revision,"publication":publication,"endpoint":endpoint,"first_parent":true,"movement":!raw,"ignore_whitespace":!raw,"runs":runs,"runs_total":total,"truncated":runs.len()<total,"tokenizer":"o200k_base"});
        let text = budget.encode(&value)?;
        if budget.fits(&text) && (!runs.is_empty() || total == 0) {
            return Ok(text);
        }
        if runs.pop().is_none() {
            anyhow::bail!("budget_too_small: attribution run does not fit")
        }
    }
}

fn ensure_captured_head(history: &History) -> Result<()> {
    ensure!(
        history.repository.resolve("HEAD")? == history.tip,
        "history_changed: HEAD moved during supplied-buffer blame; retry with a new captured tip"
    );
    Ok(())
}

fn published_source(
    store: &Store,
    target: &targets::Target,
) -> Result<(Vec<u8>, String, crate::store::Publication)> {
    store.with_publication(|conn, publication| {
        let revision: Option<String> = conn
            .query_row(
                "SELECT revision FROM files WHERE path=?1",
                [&target.path],
                |row| row.get(0),
            )
            .context("history_unavailable: path has no published source")?;
        let revision = revision.context("history_unavailable: published path is excluded")?;
        if let Some(expected) = &target.revision {
            ensure!(
                expected == &revision,
                "stale_source: target revision differs from published source"
            );
        }
        let size: i64 = conn.query_row(
            "SELECT length(bytes) FROM contents WHERE revision=?1",
            [&revision],
            |row| row.get(0),
        )?;
        ensure!(
            size >= 0 && size as u64 <= MAX_BLOB_BYTES as u64,
            "history_resource_limited: published source exceeds blob limit"
        );
        let bytes: Vec<u8> = conn.query_row(
            "SELECT bytes FROM contents WHERE revision=?1",
            [&revision],
            |row| row.get(0),
        )?;
        ensure!(
            crate::identity::ContentRevision::of(&bytes)
                == crate::identity::ContentRevision::parse(&revision)?,
            "history_unavailable: published source identity mismatch"
        );
        Ok((bytes, revision, publication.clone()))
    })
}

#[cfg(test)]
mod tests;
