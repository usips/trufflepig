use super::*;

pub(super) fn scoped_hunks(
    history: &History,
    response: String,
    budget: &OutputBudget,
) -> Result<String> {
    // The persisted coordinates are readable independently if a hunk does not fit.
    let mut value: Value = serde_json::from_str(&response)?;
    let mut hunks = Vec::new();
    for hit in value["hits"].as_array().into_iter().flatten() {
        let entry: ResultEntry = serde_json::from_value(hit.clone())?;
        if let ResultEntry::Change(change) = entry {
            let mut hunk = json!({"handle":change.handle});
            for (name, side) in [("before", change.before), ("after", change.after)] {
                if let Some(side) = side {
                    let bytes = history.repository.blob(&side.blob)?;
                    let span = side.span.validate(bytes.len())?;
                    hunk[name] = json!({"start":span.start,"end":span.end,"text":display_bytes(&bytes[span.start..span.end]),"encoding":if std::str::from_utf8(&bytes[span.start..span.end]).is_ok(){"utf8"}else{"byte-escaped"}});
                }
            }
            hunks.push(hunk);
        }
    }
    loop {
        value["hunks"] = json!(hunks);
        value["hunks_truncated"] =
            json!(hunks.len() < value["hits"].as_array().map_or(0, Vec::len));
        let text = budget.encode(&value)?;
        if budget.fits(&text) {
            return Ok(text);
        }
        if hunks.pop().is_none() {
            return Ok(response);
        }
    }
}

fn display_bytes(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.into();
    }
    let mut text = String::with_capacity(bytes.len() * 4);
    for &byte in bytes {
        use std::fmt::Write;
        write!(text, "\\x{byte:02x}").expect("string write");
    }
    text
}
