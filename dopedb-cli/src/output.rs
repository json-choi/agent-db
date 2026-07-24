use std::io::{self, Write};

use serde::Serialize;

use crate::client::ClientError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputMode {
    Human,
    Json,
}

impl OutputMode {
    pub(crate) const fn from_json_flag(json: bool) -> Self {
        if json {
            Self::Json
        } else {
            Self::Human
        }
    }
}

pub(crate) fn write_json<T: Serialize>(value: &T) -> Result<(), ClientError> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, value).map_err(|_| ClientError::Internal)?;
    lock.write_all(b"\n").map_err(|_| ClientError::Internal)
}

pub(crate) fn write_human(lines: &[String]) -> Result<(), ClientError> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    for line in lines {
        let line = sanitize_human_line(line);
        lock.write_all(line.as_bytes())
            .and_then(|_| lock.write_all(b"\n"))
            .map_err(|_| ClientError::Internal)?;
    }
    Ok(())
}

fn sanitize_human_line(line: &str) -> String {
    let mut safe = String::with_capacity(line.len());
    for character in line.chars() {
        if character == '\t' || (!character.is_control() && !is_bidi_control(character)) {
            safe.push(character);
        } else {
            safe.extend(character.escape_default());
        }
    }
    safe
}

const fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_output_escapes_terminal_and_direction_controls_but_keeps_layout_tabs() {
        let safe = sanitize_human_line("name\t\u{1b}[2J\nspoof\u{202e}");
        assert!(safe.contains('\t'));
        assert!(!safe.contains('\u{1b}'));
        assert!(!safe.contains('\n'));
        assert!(!safe.contains('\u{202e}'));
        assert!(safe.contains("\\u{1b}"));
        assert!(safe.contains("\\n"));
        assert!(safe.contains("\\u{202e}"));
    }
}
