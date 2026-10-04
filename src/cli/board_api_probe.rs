//! Installer capability detection without repository, config, or router access.
use anyhow::{Result, bail};

pub(super) fn probe_board_api(args: &[String]) -> Option<Result<String>> {
    let requested = args
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--board-api-version" || arg.starts_with("--board-api-version="));
    requested.then(|| {
        if args.len() != 1 || args[0] != "--board-api-version" {
            bail!("invalid_options: --board-api-version must be the only argument");
        }
        Ok(format!("{}\n", crate::board::board_protocol::BOARD_API))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_api_capability_exits_before_dispatch_and_emission_diagnostics() {
        let args = ["--board-api-version".to_owned()];
        let expected = format!("{}\n", crate::board::board_protocol::BOARD_API);
        assert_eq!(crate::board::board_protocol::BOARD_API, 3);
        assert_eq!(crate::cli::run(&args).unwrap(), expected);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            crate::cli::emission::execute(&args, &mut stdout, &mut stderr),
            0
        );
        assert_eq!(stdout, expected.as_bytes());
        assert!(stderr.is_empty());
    }

    #[test]
    fn board_api_capability_rejects_combined_and_malformed_args_before_dispatch() {
        for args in [
            vec!["--board-api-version", "board", "show"],
            vec!["--root=/missing-capability-root", "--board-api-version"],
            vec!["--board-api-version", "--no-daemon"],
            vec!["--board-api-version", "--budget=0"],
            vec!["--board-api-version", "--help"],
            vec!["--board-api-version=true"],
        ] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(
                crate::cli::run(&args)
                    .unwrap_err()
                    .to_string()
                    .starts_with("invalid_options:")
            );
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            assert_eq!(
                crate::cli::emission::execute(&args, &mut stdout, &mut stderr),
                2
            );
            assert!(stdout.is_empty());
            assert!(
                String::from_utf8(stderr)
                    .unwrap()
                    .starts_with("invalid_options:")
            );
        }
        assert!(
            probe_board_api(&[
                "board".into(),
                "post".into(),
                "P1".into(),
                "note".into(),
                "--".into(),
                "--board-api-version".into()
            ])
            .is_none()
        );
    }
}
