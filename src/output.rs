//! Complete-response accounting against the named o200k_base tokenizer.
use anyhow::{Result, bail};
use serde::Serialize;

pub struct OutputBudget {
    tokenizer: tiktoken_rs::CoreBPE,
    pub limit: usize,
}

impl OutputBudget {
    pub fn new(limit: usize) -> Result<Self> {
        Ok(Self {
            tokenizer: tiktoken_rs::o200k_base()?,
            limit,
        })
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
}
