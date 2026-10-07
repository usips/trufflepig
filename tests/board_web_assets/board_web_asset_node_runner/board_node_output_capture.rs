use std::{
    collections::VecDeque,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    ops::Range,
};

const TAIL_LINES: usize = 200;
const OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const EXCERPT_LINE_BYTES: usize = 4096;
const NOT_OK: &[u8] = b"not ok";

struct CapturedOutputLine {
    span: Range<u64>,
    failed: bool,
}

/// Spool full streams to disk; keep only bounded line indexes in memory.
pub(super) struct NodeOutputCapture {
    output: File,
    failures: File,
    tail: VecDeque<CapturedOutputLine>,
    position: u64,
    line_start: u64,
    matched: usize,
    failed_line: bool,
    stopped: bool,
}

impl NodeOutputCapture {
    pub(super) fn new() -> io::Result<Self> {
        let directory = super::super::board_node_temporary_files::node_temporary_directory()?;
        Ok(Self {
            output: tempfile::tempfile_in(&directory)?,
            failures: tempfile::tempfile_in(&directory)?,
            tail: VecDeque::with_capacity(TAIL_LINES),
            position: 0,
            line_start: 0,
            matched: 0,
            failed_line: false,
            stopped: false,
        })
    }

    pub(super) fn push(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.output.write_all(bytes)?;
        for &byte in bytes {
            self.position += 1;
            if byte == b'\n' {
                self.finish_line()?;
            } else if !self.failed_line {
                self.matched = if byte == NOT_OK[self.matched] {
                    self.matched + 1
                } else {
                    usize::from(byte == NOT_OK[0])
                };
                self.failed_line = self.matched == NOT_OK.len();
            }
        }
        Ok(())
    }

    fn finish_line(&mut self) -> io::Result<()> {
        let line = self.line_start..self.position;
        if self.tail.len() == TAIL_LINES {
            self.tail.pop_front();
        }
        self.tail.push_back(CapturedOutputLine {
            span: line.clone(),
            failed: self.failed_line,
        });
        if self.failed_line {
            self.failures.write_all(&line.start.to_le_bytes())?;
            self.failures.write_all(&line.end.to_le_bytes())?;
        }
        self.line_start = self.position;
        self.matched = 0;
        self.failed_line = false;
        Ok(())
    }

    pub(super) fn finish(&mut self, stopped: bool) -> io::Result<()> {
        if self.line_start < self.position {
            self.finish_line()?;
        }
        self.stopped = stopped;
        Ok(())
    }

    pub(super) fn print_diagnostics(&mut self, stream: &str) -> io::Result<()> {
        let mut diagnostics = io::stderr().lock();
        writeln!(diagnostics, "{stream}:")?;
        self.write_diagnostics(&mut diagnostics, false)
    }

    fn write_diagnostics(&mut self, writer: &mut impl Write, bounded: bool) -> io::Result<()> {
        writer.write_all(b"last 200 lines:\n")?;
        for line in &self.tail {
            write_line(
                &mut self.output,
                line.span.clone(),
                writer,
                bounded || !line.failed,
            )?;
        }
        writer.write_all(b"earlier not ok lines:\n")?;
        self.failures.rewind()?;
        let tail_start = self
            .tail
            .front()
            .map_or(self.position, |line| line.span.start);
        let mut index = [0; 16];
        while self.failures.read(&mut index[..1])? != 0 {
            self.failures.read_exact(&mut index[1..])?;
            let start = u64::from_le_bytes(index[..8].try_into().unwrap());
            let end = u64::from_le_bytes(index[8..].try_into().unwrap());
            if end <= tail_start {
                write_line(&mut self.output, start..end, writer, bounded)?;
            }
        }
        if self.stopped {
            writer.write_all(b"[pipe remained open after cleanup; stopped draining]\n")?;
        }
        Ok(())
    }

    /// Preserve small outputs; larger streams return a bounded excerpt.
    pub(super) fn output_bytes(&mut self) -> io::Result<Vec<u8>> {
        if self.position <= OUTPUT_BYTES as u64 {
            let mut bytes = Vec::with_capacity(self.position as usize);
            self.output.rewind()?;
            self.output.read_to_end(&mut bytes)?;
            return Ok(bytes);
        }
        let capacity = self
            .tail
            .iter()
            .map(|line| (line.span.end - line.span.start).min(EXCERPT_LINE_BYTES as u64) as usize)
            .sum();
        let mut excerpt = BoundedExcerpt(Vec::with_capacity(capacity));
        excerpt.write_all(b"[full output exceeded 4 MiB; excerpt follows]\n")?;
        self.write_diagnostics(&mut excerpt, true)?;
        Ok(excerpt.0)
    }
}

fn write_line(
    output: &mut File,
    line: Range<u64>,
    writer: &mut impl Write,
    bounded: bool,
) -> io::Result<()> {
    output.seek(SeekFrom::Start(line.start))?;
    let length = line.end - line.start;
    let copied = if bounded {
        length.min(EXCERPT_LINE_BYTES as u64)
    } else {
        length
    };
    io::copy(&mut output.take(copied), writer)?;
    if copied < length {
        writer.write_all(b"... [line truncated]\n")?;
    }
    Ok(())
}

struct BoundedExcerpt(Vec<u8>);

impl Write for BoundedExcerpt {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let retained = bytes.len().min(OUTPUT_BYTES - self.0.len());
        self.0.extend_from_slice(&bytes[..retained]);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn bounded_diagnostics(bytes: &[u8]) -> Vec<u8> {
    let tail_start = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .count()
        .saturating_sub(TAIL_LINES);
    let selected = |(index, line): &(usize, &[u8])| {
        *index >= tail_start || line.windows(NOT_OK.len()).any(|window| window == NOT_OK)
    };
    let capacity = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .enumerate()
        .filter(selected)
        .map(|(_, line)| line.len().min(EXCERPT_LINE_BYTES) + 24)
        .sum::<usize>()
        .min(OUTPUT_BYTES);
    let mut excerpt = BoundedExcerpt(Vec::with_capacity(capacity));
    for (_, line) in bytes
        .split_inclusive(|byte| *byte == b'\n')
        .enumerate()
        .filter(selected)
    {
        let length = line.len().min(EXCERPT_LINE_BYTES);
        excerpt.write_all(&line[..length]).unwrap();
        if length < line.len() {
            excerpt.write_all(b"... [line truncated]\n").unwrap();
        }
    }
    excerpt.0
}
