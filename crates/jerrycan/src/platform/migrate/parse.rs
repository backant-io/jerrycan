//! Per-statement parsing with graceful degradation (resolved ambiguity #1):
//! tokenize once (dollar-quoted bodies are single tokens, so a top-level `;`
//! is a safe boundary), slice the source per statement, parse each slice
//! independently. Unparseable statements degrade to `Unparsed` — they feed the
//! publication recognizer and the gap report; they never abort a migration.

use sqlparser::ast::Statement;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer};

#[derive(Debug)]
pub enum RawStatement {
    Parsed {
        stmt: Box<Statement>,
        sql: String,
        line: usize,
    },
    Unparsed {
        sql: String,
        line: usize,
    },
}

/// Byte offset of the start of each 1-based line (for Location → offset math).
fn line_offsets(sql: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(sql.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

pub fn split_and_parse(sql: &str) -> Vec<RawStatement> {
    let dialect = PostgreSqlDialect {};
    let offsets = line_offsets(sql);
    let to_offset = |line: u64, col: u64| -> usize {
        offsets.get(line as usize - 1).copied().unwrap_or(0) + (col as usize - 1)
    };
    let tokens = match Tokenizer::new(&dialect, sql).tokenize_with_location() {
        Ok(t) => t,
        // A file the tokenizer rejects outright becomes one Unparsed blob.
        Err(_) => {
            return vec![RawStatement::Unparsed {
                sql: sql.trim().to_string(),
                line: 1,
            }];
        }
    };
    let mut out = Vec::new();
    let mut stmt_start: Option<usize> = None; // byte offset
    let mut stmt_line = 1usize;
    for tok in &tokens {
        let at = to_offset(tok.span.start.line, tok.span.start.column);
        match &tok.token {
            Token::Whitespace(_) => {}
            Token::SemiColon => {
                if let Some(start) = stmt_start.take() {
                    push_statement(&mut out, &sql[start..at], stmt_line);
                }
            }
            _ => {
                if stmt_start.is_none() {
                    stmt_start = Some(at);
                    stmt_line = tok.span.start.line as usize;
                }
            }
        }
    }
    if let Some(start) = stmt_start {
        push_statement(&mut out, &sql[start..], stmt_line);
    }
    out
}

fn push_statement(out: &mut Vec<RawStatement>, stmt_sql: &str, line: usize) {
    let stmt_sql = stmt_sql.trim();
    if stmt_sql.is_empty() {
        return;
    }
    match Parser::parse_sql(&PostgreSqlDialect {}, stmt_sql) {
        Ok(mut stmts) if stmts.len() == 1 => out.push(RawStatement::Parsed {
            stmt: Box::new(stmts.remove(0)),
            sql: stmt_sql.to_string(),
            line,
        }),
        _ => out.push(RawStatement::Unparsed {
            sql: stmt_sql.to_string(),
            line,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"
create table public.todos (id uuid primary key, title text not null);

create function public.touch() returns trigger as $$
begin
  new.updated_at := now(); return new; -- note: this ; inside the body must not split it
end;
$$ language plpgsql;

alter publication supabase_realtime add table public.todos;
"#;

    #[test]
    fn splits_on_top_level_semicolons_and_survives_dollar_quoting() {
        let stmts = split_and_parse(DUMP);
        assert_eq!(stmts.len(), 3, "{stmts:?}");
        assert!(
            matches!(&stmts[0], RawStatement::Parsed { .. }),
            "CREATE TABLE parses"
        );
        // ALTER PUBLICATION is not in sqlparser's grammar → degrades, never aborts.
        match &stmts[2] {
            RawStatement::Unparsed { sql, line } => {
                assert!(sql.contains("supabase_realtime"));
                assert_eq!(*line, 10, "1-based line of the statement start");
            }
            other => panic!("expected Unparsed, got {other:?}"),
        }
    }

    #[test]
    fn a_dollar_quoted_body_stays_one_statement() {
        let stmts = split_and_parse(DUMP);
        let fn_sql = match &stmts[1] {
            RawStatement::Parsed { sql, .. } | RawStatement::Unparsed { sql, .. } => sql,
        };
        assert!(
            fn_sql.contains("language plpgsql"),
            "body + tail intact: {fn_sql}"
        );
    }
}
