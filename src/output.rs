//! Complete-response accounting against the named o200k_base tokenizer.
pub mod lines;

use anyhow::{Result, bail};
use serde::Serialize;

/// Serialization a page verb renders and budgets: JSON objects or tab lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Json,
    Lines,
}

impl OutputFormat {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "json" => Ok(Self::Json),
            "lines" => Ok(Self::Lines),
            other => bail!("invalid_options: unknown output format {other}"),
        }
    }
}

pub struct OutputBudget {
    tokenizer: tiktoken_rs::CoreBPE,
    pub limit: usize,
    pub format: OutputFormat,
}

impl OutputBudget {
    pub fn new(limit: usize) -> Result<Self> {
        Ok(Self {
            tokenizer: tiktoken_rs::o200k_base()?,
            limit,
            format: OutputFormat::Json,
        })
    }

    /// Selects the serialization page verbs fit; `render` stays JSON regardless.
    pub fn with_format(mut self, format: OutputFormat) -> Self {
        self.format = format;
        self
    }

    pub fn encode(&self, value: &impl Serialize) -> Result<String> {
        let mut text = serde_json::to_string(value)?;
        text.push('\n');
        Ok(text)
    }

    pub fn fits(&self, text: &str) -> bool {
        self.tokenizer.encode_ordinary(text).len() <= self.limit
    }

    pub fn render(&self, value: &impl Serialize) -> Result<String> {
        let text = self.encode(value)?;
        if !self.fits(&text) {
            bail!(
                "budget_too_small: complete response exceeds {} o200k_base tokens",
                self.limit
            );
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_complete_serialization() {
        let budget = OutputBudget::new(8).unwrap();
        assert!(budget.render(&serde_json::json!({"x":"hi"})).is_ok());
        assert!(
            budget
                .render(&serde_json::json!({"a very long header":"repeated words ".repeat(20)}))
                .is_err()
        );
        assert!(
            OutputBudget::new(0)
                .unwrap()
                .render(&serde_json::json!({}))
                .is_err()
        );
    }

    #[test]
    fn format_parses_known_names_only() {
        assert_eq!(OutputFormat::parse("json").unwrap(), OutputFormat::Json);
        assert_eq!(OutputFormat::parse("lines").unwrap(), OutputFormat::Lines);
        assert!(OutputFormat::parse("yaml").is_err());
        assert_eq!(
            OutputBudget::new(1)
                .unwrap()
                .with_format(OutputFormat::Lines)
                .format,
            OutputFormat::Lines
        );
    }
}
