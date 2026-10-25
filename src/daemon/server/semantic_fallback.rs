use crate::{
    cli::{normalized_args, parse},
    search::Query,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use tp_core::output::{OutputBudget, OutputFormat, lines_format::COVERAGE_KEY};

const FALLBACK_BUDGET_RESERVE: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SemanticAdmissionFallback {
    pub outstanding: usize,
    pub limit: usize,
    budget: usize,
    format: OutputFormat,
}

impl SemanticAdmissionFallback {
    pub(super) fn from_request(
        args: &[String],
        outstanding: usize,
        limit: usize,
    ) -> Result<(Self, Vec<String>)> {
        ensure!(
            limit > 0 && outstanding >= limit,
            "invalid_semantic_root_limit"
        );
        let mut options = parse(args).context("semantic_fallback_arguments_invalid")?;
        ensure!(
            options.words.first().is_some_and(|verb| verb == "search"),
            "semantic_fallback_requires_search"
        );
        let query_text = options.words[1..].join(" ");
        let query = Query::parse(&query_text)?;
        ensure!(
            query.semantic_eligible() && !query.regex && !query_text.starts_with("refs:"),
            "semantic_fallback_requires_free_text_search"
        );
        ensure!(
            options.budget > FALLBACK_BUDGET_RESERVE,
            coded!(
                SemanticFallbackBudgetTooSmall,
                "--budget must exceed {FALLBACK_BUDGET_RESERVE}"
            )
        );

        let fallback = Self {
            outstanding,
            limit,
            budget: options.budget,
            format: options.output_format(),
        };
        options.budget -= FALLBACK_BUDGET_RESERVE;
        options.sem = false;
        options.rerank = false;
        options.no_sem = true;
        options.no_rerank = true;
        let fallback_args = normalized_args(&options, &options.root);
        Ok((fallback, fallback_args))
    }

    pub(super) fn annotate(&self, output: &str) -> Result<String> {
        let budget = OutputBudget::new(self.budget)
            .context("semantic_fallback_output_budget_invalid")?
            .with_format(self.format);
        match self.format {
            OutputFormat::Json => self.annotate_json(output, &budget),
            OutputFormat::Lines => self.annotate_lines(output, &budget),
        }
    }

    fn annotate_json(&self, output: &str, budget: &OutputBudget) -> Result<String> {
        let mut value = serde_json::from_str::<Value>(output)
            .map_err(|error| anyhow!(coded!(SemanticFallbackOutputInvalid, "{error}")))?;
        ensure!(
            value.get("hits").and_then(Value::as_array).is_some(),
            coded!(SemanticFallbackOutputInvalid, "expected search page hits")
        );
        let Some(coverage) = value.get("coverage") else {
            bail!(coded!(
                SemanticFallbackOutputMissingCoverage,
                "expected search page coverage"
            ));
        };
        let object_coverage = coverage.is_object();
        let array_coverage = coverage.is_array();
        let admission = json!({
            "semantic_status": "skipped",
            "semantic_reason": "root_limit",
            "rerank_status": "skipped",
            "rerank_reason": "root_limit",
            "cause": "root_limit",
            "outstanding": self.outstanding,
            "limit": self.limit,
        });
        if object_coverage {
            let coverage = value
                .get_mut("coverage")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| {
                    anyhow!(coded!(
                        SemanticFallbackOutputMissingCoverage,
                        "expected object"
                    ))
                })?;
            coverage.insert("semantic_status".into(), json!("skipped"));
            coverage.insert("semantic_reason".into(), json!("root_limit"));
            coverage.insert("rerank_status".into(), json!("skipped"));
            coverage.insert("rerank_reason".into(), json!("root_limit"));
            coverage.insert(
                "semantic_admission".into(),
                json!({
                    "cause": "root_limit",
                    "outstanding": self.outstanding,
                    "limit": self.limit,
                }),
            );
        } else if array_coverage {
            value
                .as_object_mut()
                .ok_or_else(|| {
                    anyhow!(coded!(
                        SemanticFallbackOutputInvalid,
                        "expected object search page"
                    ))
                })?
                .insert("semantic_admission".into(), admission);
        } else {
            bail!(coded!(
                SemanticFallbackOutputMissingCoverage,
                "expected object or array"
            ));
        }
        budget.render(&value).map_err(|error| {
            anyhow!(coded!(
                SemanticFallbackOutputTooLarge,
                "fallback coverage does not fit --budget {}: {error:#}",
                self.budget
            ))
        })
    }

    fn annotate_lines(&self, output: &str, budget: &OutputBudget) -> Result<String> {
        let mut annotated = String::with_capacity(output.len() + 128);
        let mut found_coverage = false;
        for line in output.split_inclusive('\n') {
            if !found_coverage && line.starts_with(COVERAGE_KEY) {
                let has_newline = line.ends_with('\n');
                let text = line.strip_suffix('\n').unwrap_or(line);
                annotated.push_str(text);
                annotated.push_str(&format!(
                    "; semantic_status=skipped; semantic_reason=root_limit; rerank_status=skipped; rerank_reason=root_limit; semantic_admission_outstanding={}; semantic_admission_limit={}",
                    self.outstanding, self.limit
                ));
                if has_newline {
                    annotated.push('\n');
                }
                found_coverage = true;
            } else {
                annotated.push_str(line);
            }
        }
        ensure!(
            found_coverage,
            coded!(
                SemanticFallbackOutputMissingCoverage,
                "expected search page coverage line"
            )
        );
        ensure!(
            budget.fits(&annotated),
            coded!(
                SemanticFallbackOutputTooLarge,
                "fallback coverage does not fit --budget {}",
                self.budget
            )
        );
        Ok(annotated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_args(root: &std::path::Path, format: &str, budget: usize) -> Vec<String> {
        vec![
            "--root".into(),
            root.to_string_lossy().into_owned(),
            "--sem".into(),
            "--rerank".into(),
            "--format".into(),
            format.into(),
            "--budget".into(),
            budget.to_string(),
            "search".into(),
            "authentication".into(),
            "kind:file".into(),
        ]
    }

    #[test]
    fn fallback_args_remove_enabled_stages_and_keep_the_search_request() {
        let root = tempfile::tempdir().unwrap();
        let args = request_args(root.path(), "json", 1200);
        let (fallback, lexical_args) =
            SemanticAdmissionFallback::from_request(&args, 2, 2).unwrap();

        assert_eq!((fallback.outstanding, fallback.limit), (2, 2));
        assert_eq!(fallback.budget, 1200);
        assert!(!lexical_args.iter().any(|arg| arg == "--sem"));
        assert!(!lexical_args.iter().any(|arg| arg == "--rerank"));
        assert!(lexical_args.iter().any(|arg| arg == "--no-sem"));
        assert!(lexical_args.iter().any(|arg| arg == "--no-rerank"));
        assert!(lexical_args.iter().any(|arg| arg == "authentication"));
        assert!(lexical_args.iter().any(|arg| arg == "kind:file"));
        let options = parse(&lexical_args).unwrap();
        assert_eq!(options.budget, 1200 - FALLBACK_BUDGET_RESERVE);
        assert!(!options.sem);
        assert!(!options.rerank);
        assert!(
            crate::semantic::SemanticSession::new()
                .prepare(options.sem, root.path(), "authentication")
                .unwrap()
                .is_none()
        );
        let classified = crate::cli::classify_daemon_request(&lexical_args).unwrap();
        assert_eq!(
            classified.class.lane,
            tp_core::request_pool::RequestLane::Query
        );
        assert!(classified.roots.is_empty());
    }

    #[test]
    fn json_annotation_preserves_hits_and_adds_stable_admission_coverage() {
        let fallback = SemanticAdmissionFallback {
            outstanding: 2,
            limit: 2,
            budget: 1200,
            format: OutputFormat::Json,
        };
        let output = r#"{"coverage":{"indexed_files":3,"custom":"kept"},"hits":[{"path":"src/auth.rs"}],"next":null}"#;

        let annotated: Value = serde_json::from_str(&fallback.annotate(output).unwrap()).unwrap();
        assert_eq!(annotated["hits"][0]["path"], "src/auth.rs");
        assert_eq!(annotated["coverage"]["indexed_files"], 3);
        assert_eq!(annotated["coverage"]["custom"], "kept");
        assert_eq!(annotated["coverage"]["semantic_status"], "skipped");
        assert_eq!(annotated["coverage"]["semantic_reason"], "root_limit");
        assert_eq!(annotated["coverage"]["rerank_status"], "skipped");
        assert_eq!(annotated["coverage"]["rerank_reason"], "root_limit");
        assert_eq!(
            annotated["coverage"]["semantic_admission"]["outstanding"],
            2
        );
        assert_eq!(annotated["coverage"]["semantic_admission"]["limit"], 2);
    }

    #[test]
    fn workspace_annotation_preserves_coverage_array_and_adds_one_global_marker() {
        let fallback = SemanticAdmissionFallback {
            outstanding: 3,
            limit: 3,
            budget: 1200,
            format: OutputFormat::Json,
        };
        let output = r#"{"workspace":"project","coverage":[{"member":"home","issues":{"semantic_status":"ready"}},{"member":"lib"}],"hits":[{"path":"src/auth.rs"}],"next":null}"#;

        let annotated: Value = serde_json::from_str(&fallback.annotate(output).unwrap()).unwrap();
        assert_eq!(annotated["hits"][0]["path"], "src/auth.rs");
        assert_eq!(annotated["coverage"][0]["member"], "home");
        assert_eq!(
            annotated["coverage"][0]["issues"]["semantic_status"],
            "ready"
        );
        assert_eq!(annotated["coverage"][1]["member"], "lib");
        assert_eq!(
            annotated["semantic_admission"]["semantic_status"],
            "skipped"
        );
        assert_eq!(
            annotated["semantic_admission"]["semantic_reason"],
            "root_limit"
        );
        assert_eq!(annotated["semantic_admission"]["rerank_status"], "skipped");
        assert_eq!(
            annotated["semantic_admission"]["rerank_reason"],
            "root_limit"
        );
        assert_eq!(annotated["semantic_admission"]["outstanding"], 3);
        assert_eq!(annotated["semantic_admission"]["limit"], 3);
    }

    #[test]
    fn fallback_rejects_budget_that_cannot_hold_reserved_coverage() {
        let root = tempfile::tempdir().unwrap();
        let args = request_args(root.path(), "json", FALLBACK_BUDGET_RESERVE);

        let error = SemanticAdmissionFallback::from_request(&args, 2, 2).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("semantic_fallback_budget_too_small")
        );
    }

    #[test]
    fn line_annotation_keeps_pagination_and_shows_root_limit_counts() {
        let fallback = SemanticAdmissionFallback {
            outstanding: 2,
            limit: 2,
            budget: 1200,
            format: OutputFormat::Lines,
        };
        let output = "src/auth.rs\ncoverage: indexed 3/3\nnext: more abc@1\n";

        let annotated = fallback.annotate(output).unwrap();
        assert!(annotated.contains("semantic_status=skipped"));
        assert!(annotated.contains("semantic_reason=root_limit"));
        assert!(annotated.contains("rerank_status=skipped"));
        assert!(annotated.contains("rerank_reason=root_limit"));
        assert!(annotated.contains("semantic_admission_outstanding=2"));
        assert!(annotated.contains("semantic_admission_limit=2"));
        assert!(annotated.contains("next: more abc@1\n"));
    }

    #[test]
    fn annotation_fails_clearly_when_coverage_does_not_fit() {
        let fallback = SemanticAdmissionFallback {
            outstanding: 2,
            limit: 2,
            budget: 1,
            format: OutputFormat::Lines,
        };

        let error = fallback
            .annotate("src/auth.rs\ncoverage: indexed 3/3\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("semantic_fallback_output_too_large"));
    }
}
