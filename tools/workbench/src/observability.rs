use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{anyhow, Context, Result};

use crate::contract::{LogEntry, LogLevel, LogPage, SCHEMA_VERSION};

pub(crate) const DEFAULT_PAGE_LIMIT: usize = 200;
pub(crate) const MAX_PAGE_LIMIT: usize = 500;

const MAX_LOG_LINE_BYTES: usize = 16 * 1024;
const MAX_TAIL_SCAN_BYTES: u64 = 512 * 1024;

pub(crate) fn read_log_page(
    path: &Path,
    device_id: String,
    cursor: Option<u64>,
    limit: usize,
) -> Result<LogPage> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LogPage {
                schema_version: SCHEMA_VERSION,
                device_id,
                file_size: 0,
                next_cursor: 0,
                reset: cursor.is_some_and(|value| value != 0),
                has_more: false,
                entries: Vec::new(),
            });
        }
        Err(error) => {
            return Err(error).with_context(|| format!("open daemon log {}", path.display()));
        }
    };
    let file_size = file
        .metadata()
        .with_context(|| format!("inspect daemon log {}", path.display()))?
        .len();
    let (start, reset) = match cursor {
        Some(value) if value <= file_size => (value, false),
        Some(_) => (tail_start(&mut file, file_size, limit)?, true),
        None => (tail_start(&mut file, file_size, limit)?, false),
    };
    file.seek(SeekFrom::Start(start))
        .with_context(|| format!("seek daemon log {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut entries = Vec::with_capacity(limit);
    let mut next_cursor = start;
    while entries.len() < limit && next_cursor < file_size {
        let entry_cursor = next_cursor;
        let (line, truncated) = read_bounded_line(&mut reader)?;
        next_cursor = reader.stream_position()?;
        if next_cursor == entry_cursor {
            break;
        }
        entries.push(parse_log_entry(entry_cursor, &line, truncated));
    }
    Ok(LogPage {
        schema_version: SCHEMA_VERSION,
        device_id,
        file_size,
        next_cursor,
        reset,
        has_more: next_cursor < file_size,
        entries,
    })
}

fn tail_start(file: &mut File, file_size: u64, limit: usize) -> Result<u64> {
    if file_size == 0 {
        return Ok(0);
    }
    let scan_start = file_size.saturating_sub(MAX_TAIL_SCAN_BYTES);
    file.seek(SeekFrom::Start(scan_start))?;
    let scan_len = usize::try_from(file_size.saturating_sub(scan_start))
        .context("daemon log tail exceeds addressable memory")?;
    let mut bytes = vec![0; scan_len];
    file.read_exact(&mut bytes)?;

    let mut before = bytes.len();
    if bytes.last() == Some(&b'\n') {
        before = before.saturating_sub(1);
    }
    let mut remaining = limit;
    while remaining > 0 {
        let Some(prefix) = bytes.get(..before) else {
            return Err(anyhow!("invalid daemon log tail boundary"));
        };
        let Some(position) = prefix.iter().rposition(|byte| *byte == b'\n') else {
            if scan_start == 0 {
                return Ok(0);
            }
            let Some(position) = bytes.iter().position(|byte| *byte == b'\n') else {
                return Ok(file_size);
            };
            let position = u64::try_from(position).context("daemon log tail position overflow")?;
            return Ok(scan_start.saturating_add(position).saturating_add(1));
        };
        remaining = remaining.saturating_sub(1);
        before = position;
    }
    let before = u64::try_from(before).context("daemon log tail position overflow")?;
    Ok(scan_start.saturating_add(before).saturating_add(1))
}

fn read_bounded_line(reader: &mut BufReader<File>) -> Result<(Vec<u8>, bool)> {
    let mut line = Vec::new();
    let mut truncated = false;
    loop {
        let (consumed, finished) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok((line, truncated));
            }
            let consumed = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |position| position.saturating_add(1));
            let remaining = MAX_LOG_LINE_BYTES.saturating_sub(line.len());
            let copied = consumed.min(remaining);
            let copied_bytes = available
                .get(..copied)
                .ok_or_else(|| anyhow!("invalid daemon log line boundary"))?;
            line.extend_from_slice(copied_bytes);
            if copied < consumed {
                truncated = true;
            }
            (
                consumed,
                available.get(consumed.saturating_sub(1)) == Some(&b'\n'),
            )
        };
        reader.consume(consumed);
        if finished {
            break;
        }
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    Ok((line, truncated))
}

fn parse_log_entry(cursor: u64, bytes: &[u8], truncated: bool) -> LogEntry {
    let clean = strip_ansi(&String::from_utf8_lossy(bytes));
    let clean = clean.trim();
    let mut parts = clean.split_whitespace();
    let timestamp = parts.next().filter(|value| looks_like_timestamp(value));
    let level_text = timestamp.and_then(|_| parts.next());
    let remainder = if level_text.is_some() {
        parts.collect::<Vec<_>>().join(" ")
    } else {
        clean.to_owned()
    };
    let remainder = remainder.trim();
    let level = level_text.map_or(LogLevel::Unknown, parse_level);
    let (target, message) = if timestamp.is_some() && level_text.is_some() {
        remainder
            .split_once(": ")
            .map_or((None, remainder), |(target, message)| {
                (Some(target.to_owned()), message)
            })
    } else {
        (None, remainder)
    };
    LogEntry {
        cursor,
        timestamp: timestamp.map(str::to_owned),
        level,
        target,
        message: message.to_owned(),
        truncated,
    }
}

fn looks_like_timestamp(value: &str) -> bool {
    value.len() >= 20 && value.contains('T') && value.ends_with('Z')
}

fn parse_level(value: &str) -> LogLevel {
    match value.to_ascii_uppercase().as_str() {
        "TRACE" => LogLevel::Trace,
        "DEBUG" => LogLevel::Debug,
        "INFO" => LogLevel::Info,
        "WARN" => LogLevel::Warn,
        "ERROR" => LogLevel::Error,
        _ => LogLevel::Unknown,
    }
}

fn strip_ansi(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' && characters.peek() == Some(&'[') {
            characters.next();
            for code in characters.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
            continue;
        }
        output.push(character);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn parses_colored_tracing_lines_into_fields() {
        let line = b"\x1b[2m2026-08-09T23:02:54.347526Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mlait::daemon::host\x1b[0m\x1b[2m:\x1b[0m online";
        let entry = parse_log_entry(7, line, false);
        assert_eq!(entry.cursor, 7);
        assert_eq!(
            entry.timestamp.as_deref(),
            Some("2026-08-09T23:02:54.347526Z")
        );
        assert!(matches!(entry.level, LogLevel::Info));
        assert_eq!(entry.target.as_deref(), Some("lait::daemon::host"));
        assert_eq!(entry.message, "online");
    }

    #[test]
    fn defaults_to_a_bounded_tail_and_supports_cursors() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("daemon.log");
        let mut file = File::create(&path).expect("create");
        for number in 0..12 {
            writeln!(file, "line {number}").expect("write");
        }
        drop(file);

        let tail = read_log_page(&path, "alice".into(), None, 3).expect("tail");
        assert_eq!(tail.entries.len(), 3);
        assert_eq!(tail.entries[0].message, "line 9");
        let cursor = tail.next_cursor;

        let caught_up = read_log_page(&path, "alice".into(), Some(cursor), 3).expect("cursor");
        assert!(caught_up.entries.is_empty());
        assert!(!caught_up.has_more);
    }

    #[test]
    fn a_cursor_past_a_truncated_file_is_reset_to_the_tail() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("daemon.log");
        std::fs::write(&path, b"one\ntwo\n").expect("write");
        let page = read_log_page(&path, "alice".into(), Some(999), 1).expect("page");
        assert!(page.reset);
        assert_eq!(page.entries[0].message, "two");
    }
}
