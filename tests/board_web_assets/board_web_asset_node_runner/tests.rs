use super::append_output_diagnostics;
use super::board_node_output_capture::NodeOutputCapture;

#[test]
fn failure_diagnostics_keep_last_200_lines_and_every_not_ok() {
    let mut stdout = String::from("STDOUT_TOO_OLD\nnot ok 1 - early stdout failure\n");
    let mut stderr = String::from("STDERR_TOO_OLD\nnot ok 2 - early stderr failure\n");
    for number in 0..250 {
        stdout.push_str(&format!("stdout tail {number}\n"));
        stderr.push_str(&format!("stderr tail {number}\n"));
    }
    let diagnostics = append_output_diagnostics(
        "node timed out".to_owned(),
        Ok((stdout.into_bytes(), stderr.into_bytes())),
    );
    assert!(diagnostics.contains("not ok 1 - early stdout failure"));
    assert!(diagnostics.contains("not ok 2 - early stderr failure"));
    assert!(diagnostics.contains("stdout tail 50\n"));
    assert!(diagnostics.contains("stderr tail 50\n"));
    assert!(diagnostics.contains("stdout tail 249\n"));
    assert!(diagnostics.contains("stderr tail 249\n"));
    assert!(!diagnostics.contains("STDOUT_TOO_OLD"));
    assert!(!diagnostics.contains("STDERR_TOO_OLD"));
    assert!(!diagnostics.contains("stdout tail 49\n"));
    assert!(!diagnostics.contains("stderr tail 49\n"));
}

#[test]
fn large_stream_keeps_tail_and_not_ok_without_retaining_full_output() {
    let mut capture = NodeOutputCapture::new().expect("temporary output spool");
    capture.push(b"OLD_ORDINARY_OUTPUT\nnot").unwrap();
    capture.push(b" ok 1 - early split failure\n").unwrap();
    let mut line = [b'x'; 128];
    line[127] = b'\n';
    for _ in 0..40_000 {
        capture.push(&line).unwrap();
    }
    capture.push(b"FINAL_OUTPUT_MARKER\n").unwrap();
    capture.finish(false).unwrap();
    let output = capture.output_bytes().unwrap();
    assert!(output.len() < 64 * 1024, "full noisy stream was retained");
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("not ok 1 - early split failure"));
    assert!(output.contains("FINAL_OUTPUT_MARKER"));
    assert!(!output.contains("OLD_ORDINARY_OUTPUT"));
}
