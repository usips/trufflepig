//! Registry listing is independent of workspace discovery and index state.

use super::{Arguments, OutputBudget, config, encode_path};
use anyhow::{Result, ensure};
use serde_json::json;
use std::fmt::Write;

pub(crate) fn list(options: &Arguments) -> Result<String> {
    ensure!(options.words.len() == 2, "usage: ws list");
    let workspaces = config::registered_workspaces()?;
    let budget = OutputBudget::new(options.budget)?;
    if options.json {
        let rows: Vec<_> = workspaces
            .iter()
            .map(|workspace| {
                let members: Vec<_> = workspace
                    .members
                    .iter()
                    .map(|member| {
                        json!({"name": member.name, "root": encode_path(&member.root),
                        "available": member.available})
                    })
                    .collect();
                json!({"name": workspace.name, "id": workspace.id,
                    "config_path": encode_path(&workspace.config_path), "members": members,
                    "error": workspace.error})
            })
            .collect();
        return budget.render(&json!({"workspaces": rows}));
    }
    let capacity = workspaces
        .iter()
        .map(|workspace| {
            workspace.name.len()
                + workspace.id.len()
                + workspace.config_path.as_os_str().len() * 3
                + workspace.error.as_ref().map_or(0, String::len)
                + 32
                + workspace
                    .members
                    .iter()
                    .map(|member| member.name.len() + member.root.as_os_str().len() * 3 + 32)
                    .sum::<usize>()
        })
        .sum();
    let mut output = String::with_capacity(capacity);
    for workspace in &workspaces {
        let availability = if workspace.error.is_some() {
            "unavailable"
        } else {
            "available"
        };
        writeln!(
            output,
            "{}\t{}\t{}\t{availability}",
            workspace.name.escape_debug(),
            workspace.id,
            encode_path(&workspace.config_path),
        )?;
        for member in &workspace.members {
            let availability = if member.available {
                "available"
            } else {
                "unavailable"
            };
            writeln!(
                output,
                "  member\t{}\t{}\t{availability}",
                member.name,
                encode_path(&member.root),
            )?;
        }
        if let Some(error) = &workspace.error {
            writeln!(output, "  error: {}", error.escape_debug())?;
        }
    }
    ensure!(
        budget.fits(&output),
        "budget_too_small: complete response exceeds {} o200k_base tokens",
        budget.limit
    );
    Ok(output)
}
