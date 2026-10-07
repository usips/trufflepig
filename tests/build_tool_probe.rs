//! Check Cargo's compiled tool cfgs against the actual build-script report.
use std::{fs, path::Path};

#[test]
fn build_cfgs_match_recorded_tool_probes() {
    let report = fs::read_to_string(Path::new(env!("OUT_DIR")).join("board_tool_probes.txt"))
        .expect("the build script writes its actual tool probe outcomes");
    let outcome = |tool| {
        report
            .lines()
            .filter_map(|line| line.split_once('='))
            .find(|(name, _)| *name == tool)
            .expect("probe report contains the tool outcome")
            .1
    };
    let found_version = |tool| {
        outcome(tool)
            .strip_prefix("found ")
            .and_then(|found| found.rsplit_once(" (").map(|(_, version)| version))
            .and_then(|version| version.strip_suffix(')'))
    };

    let git = found_version("git").map(|version| {
        let (major, minor) = version
            .split_once('.')
            .expect("Git report contains major.minor");
        (major.parse::<u32>().unwrap(), minor.parse::<u32>().unwrap())
    });
    assert_eq!(
        cfg!(board_git_2_46),
        git.is_some_and(|version| version >= (2, 46))
    );
    assert_eq!(
        cfg!(board_git_2_55),
        git.is_some_and(|version| version >= (2, 55))
    );
    let node = found_version("node").map(|version| {
        version
            .strip_prefix('v')
            .expect("Node report contains v<major>")
            .parse::<u32>()
            .unwrap()
    });
    assert_eq!(cfg!(board_node_20), node.is_some_and(|major| major >= 20));
}
