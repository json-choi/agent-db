//! SQL script scanning shared by the SQL screen's multi-statement executor.
//! The scanner is deliberately syntax-light: it only finds top-level statement
//! boundaries while preserving quoted strings, dollar quotes, and comments.

use crate::model::Engine;

/// Split a SQL script into top-level statements for the selected engine.
///
/// MySQL honors backslash escapes in quoted literals by default. PostgreSQL and
/// SQLite use the plain doubled-quote scan.
pub(crate) fn split_statements(sql: &str, engine: Engine) -> Vec<String> {
    split_statements_impl(sql, matches!(engine, Engine::Mysql))
}

fn split_statements_impl(sql: &str, backslash_escapes: bool) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"') => {
                i += 1;
                while i < bytes.len() {
                    if backslash_escapes && bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b'$' => {
                if let Some(len) = dollar_tag_len(bytes, i) {
                    let tag = &bytes[i..i + len];
                    i += len;
                    while i < bytes.len() {
                        if bytes[i..].starts_with(tag) {
                            i += len;
                            break;
                        }
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            b';' => {
                push_statement(&mut out, &sql[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }

    push_statement(&mut out, &sql[start..]);
    out
}

fn push_statement(out: &mut Vec<String>, raw: &str) {
    let statement = raw.trim();
    if !statement.is_empty() && is_effective(statement) {
        out.push(statement.to_string());
    }
}

/// True when a split segment contains executable SQL rather than comments only.
pub(crate) fn is_effective(statement: &str) -> bool {
    !strip_comments(statement).trim().is_empty()
}

fn strip_comments(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn dollar_tag_len(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        j += 1;
    }
    (j < bytes.len() && bytes[j] == b'$').then_some(j - i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quotes_comments_and_dollar_blocks() {
        let sql = r#"
            -- setup
            SELECT 'semi;colon';
            /* block; comment */
            DO $$ BEGIN RAISE NOTICE 'inside;block'; END $$;
            SELECT 2;
        "#;
        let statements = split_statements(sql, Engine::Postgres);
        assert_eq!(statements.len(), 3);
        assert!(statements[0].contains("semi;colon"));
        assert!(statements[1].contains("inside;block"));
        assert!(statements[2].contains("SELECT 2"));
    }

    #[test]
    fn drops_comment_only_segments() {
        let sql = "-- note only;\n/* another note */;\nSELECT 1;";
        let statements = split_statements(sql, Engine::Sqlite);
        assert_eq!(statements, ["SELECT 1"]);
        assert!(!is_effective("-- note\n/* block */"));
        assert!(is_effective("-- lead\nSELECT 1"));
    }

    #[test]
    fn mysql_backslash_escape_keeps_literal_whole() {
        let sql = r"INSERT INTO t VALUES ('a\';b'); INSERT INTO t VALUES ('c');";
        let statements = split_statements(sql, Engine::Mysql);
        assert_eq!(statements.len(), 2);
        assert!(statements[0].contains(r"'a\';b'"));
    }

    #[test]
    fn preserves_doubled_quote_escape() {
        let sql = "SELECT 'it''s; fine'; SELECT \"semi;column\";";
        let statements = split_statements(sql, Engine::Postgres);
        assert_eq!(statements.len(), 2);
    }
}
