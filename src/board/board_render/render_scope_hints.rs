//! CLI continuation flags retain the caller's project selector.
use crate::board::board_protocol::ReadScope;

pub(super) fn cli_scope_flag(scope: &ReadScope, project: Option<&str>) -> String {
    if let Some(selector) = project {
        let quoted = selector.is_empty()
            || !selector
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
        let quotes = selector.bytes().filter(|byte| *byte == b'\'').count();
        let mut flag =
            String::with_capacity(11 + selector.len() + usize::from(quoted) * (2 + quotes * 3));
        flag.push_str(" --project ");
        if quoted {
            flag.push('\'');
            for character in selector.chars() {
                if character == '\'' {
                    flag.push_str("'\\''");
                } else {
                    flag.push(character);
                }
            }
            flag.push('\'');
        } else {
            flag.push_str(selector);
        }
        return flag;
    }
    match scope {
        ReadScope::All => " --all".into(),
        ReadScope::Unscoped => " --project unscoped".into(),
        ReadScope::Repo(_) | ReadScope::Keys(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn original_project_selector_is_quoted_without_inference() {
        let scope = ReadScope::Keys(BTreeSet::new());
        assert_eq!(cli_scope_flag(&scope, Some("W1")), " --project W1");
        assert_eq!(cli_scope_flag(&scope, Some("-Dash")), " --project -Dash");
        assert_eq!(
            cli_scope_flag(&scope, Some("project's $(value)")),
            " --project 'project'\\''s $(value)'"
        );
        assert_eq!(cli_scope_flag(&scope, Some("")), " --project ''");
        assert_eq!(cli_scope_flag(&scope, None), "");
    }

    #[test]
    fn scope_only_hints_preserve_all_and_unscoped_reads() {
        assert_eq!(cli_scope_flag(&ReadScope::All, None), " --all");
        assert_eq!(
            cli_scope_flag(&ReadScope::Unscoped, None),
            " --project unscoped"
        );
        assert_eq!(cli_scope_flag(&ReadScope::All, Some("W1")), " --project W1");
    }
}
