//! `pg_compat` — PostgreSQL pg_dump SQL parser and translator.
//!
//! Translates PG-flavored SQL to QMvir-compatible SQL for import.
//! Key transforms:
//! - `INSERT INTO t VALUES (...)` → `INSERT INTO t (c1,c2,...) VALUES (...)`
//!   (QMvir requires explicit column names)
//! - Strips PG-specific syntax (SERIAL, DEFAULT, sequences, etc.)

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use crate::gateway::native_sql::ColType;

/// Parsed PG dump contents.
#[derive(Clone, Debug, Default)]
pub struct PgDump {
    pub tables: Vec<PgTable>,
    pub inserts: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PgTable {
    pub name: String,
    pub columns: Vec<PgColumn>,
}

#[derive(Clone, Debug)]
pub struct PgColumn {
    pub name: String,
    pub pg_type: String,
}

/// Parse a pg_dump SQL file into structured commands.
pub fn parse_pgdump(path: &Path) -> io::Result<PgDump> {
    let content = fs::read_to_string(path)?;
    let mut dump = PgDump::default();
    let mut table_map: HashMap<String, PgTable> = HashMap::new();
    let mut statement = String::new();
    let mut lines = content.lines();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("--") || trimmed.starts_with("\\connect") {
            continue;
        }

        if is_copy_header(trimmed) {
            if !statement.trim().is_empty() {
                process_statement(&statement, &mut dump, &mut table_map)?;
                statement.clear();
            }

            let (table_name, columns) = parse_copy_header(trimmed)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let mut rows = Vec::new();
            let mut terminated = false;

            for copy_line in lines.by_ref() {
                if copy_line.trim() == "\\." {
                    terminated = true;
                    break;
                }
                rows.push(copy_line.to_string());
            }

            if !terminated {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unterminated COPY block for table '{table_name}'"),
                ));
            }

            let table = table_map.get(&table_name);
            let translated = translate_copy_block(&table_name, &columns, table, &rows)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            dump.inserts.extend(translated);
            continue;
        }

        if !statement.is_empty() {
            statement.push('\n');
        }
        statement.push_str(line);

        if trimmed.ends_with(';') {
            process_statement(&statement, &mut dump, &mut table_map)?;
            statement.clear();
        }
    }

    if !statement.trim().is_empty() {
        process_statement(&statement, &mut dump, &mut table_map)?;
    }

    Ok(dump)
}

/// Translate a PG INSERT statement to QMvir-compatible SQL.
pub fn translate_insert(sql: &str, columns: &[String]) -> String {
    match parse_insert_parts(sql) {
        Ok(parts) => {
            let columns = if parts.columns.is_empty() {
                if columns.is_empty() {
                    return sql.trim().trim_end_matches(';').to_string();
                }
                columns.to_vec()
            } else {
                parts.columns
            };
            let column_list = columns
                .iter()
                .map(|column| quote_ident(column))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "INSERT INTO {} ({}) VALUES {}",
                quote_ident(&parts.table_name),
                column_list,
                parts.values_part.trim().trim_end_matches(';')
            )
        }
        Err(_) => sql.trim().trim_end_matches(';').to_string(),
    }
}

pub fn create_table_sql(table: &PgTable) -> String {
    let defs = table
        .columns
        .iter()
        .map(|column| {
            format!(
                "{} {}",
                quote_ident(&column.name),
                qm_type_name(&column.pg_type)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("CREATE TABLE {} ({defs})", quote_ident(&table.name))
}

pub fn insert_table_name(sql: &str) -> Option<String> {
    parse_insert_parts(sql).ok().map(|parts| parts.table_name)
}

pub fn count_insert_rows(sql: &str) -> usize {
    parse_insert_parts(sql)
        .ok()
        .and_then(|parts| parse_value_groups(parts.values_part).ok())
        .map(|groups| groups.len())
        .unwrap_or(0)
}

fn process_statement(
    statement: &str,
    dump: &mut PgDump,
    table_map: &mut HashMap<String, PgTable>,
) -> io::Result<()> {
    let trimmed = statement.trim();
    if trimmed.is_empty() || should_skip_statement(trimmed) {
        return Ok(());
    }

    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with("CREATE TABLE") {
        let table = parse_create_table(trimmed)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        table_map.insert(table.name.clone(), table.clone());
        dump.tables.push(table);
    } else if upper.starts_with("INSERT INTO") {
        let table_name = insert_table_name(trimmed).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid INSERT statement")
        })?;
        let fallback_columns = table_map
            .get(&table_name)
            .map(|table| {
                table
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let translated =
            translate_insert_with_table(trimmed, &fallback_columns, table_map.get(&table_name))
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        dump.inserts.push(translated);
    }

    Ok(())
}

fn is_copy_header(statement: &str) -> bool {
    let upper = statement.to_ascii_uppercase();
    upper.starts_with("COPY ") && upper.contains(" FROM STDIN;")
}

fn should_skip_statement(statement: &str) -> bool {
    let upper = statement.trim().to_ascii_uppercase();
    upper.starts_with("SET ")
        || upper.starts_with("SELECT PG_CATALOG.SET_CONFIG")
        || upper.starts_with("SELECT SETVAL")
        || upper.starts_with("ALTER TABLE")
        || upper.starts_with("ALTER SEQUENCE")
        || upper.starts_with("CREATE SEQUENCE")
        || upper.starts_with("CREATE INDEX")
        || upper.starts_with("ALTER INDEX")
        || upper.starts_with("COMMENT ON")
        || upper.starts_with("GRANT ")
        || upper.starts_with("REVOKE ")
        || upper.starts_with("CREATE EXTENSION")
        || upper.starts_with("CREATE SCHEMA")
        || upper.starts_with("ALTER ROLE")
        || upper.starts_with("LOCK TABLE")
        || upper.starts_with("VACUUM")
        || upper.starts_with("ANALYZE")
        || upper.starts_with("BEGIN")
        || upper.starts_with("COMMIT")
        || upper.starts_with("RESET ")
        || upper.starts_with("COPY ")
}

fn parse_create_table(statement: &str) -> Result<PgTable, String> {
    let trimmed = statement.trim().trim_end_matches(';').trim();
    let open = trimmed
        .find('(')
        .ok_or("CREATE TABLE missing column list")?;
    let close = trimmed
        .rfind(')')
        .ok_or("CREATE TABLE missing closing parenthesis")?;
    let head = trimmed["CREATE TABLE".len()..open].trim();
    let head = strip_optional_keyword(head, "IF NOT EXISTS");
    let head = strip_optional_keyword(head, "ONLY");
    let table_name = normalize_qualified_ident(head);
    let defs = &trimmed[open + 1..close];

    let mut columns = Vec::new();
    for def in split_top_level_commas(defs) {
        let trimmed = def.trim();
        if trimmed.is_empty() {
            continue;
        }

        let upper = trimmed.to_ascii_uppercase();
        if upper.starts_with("CONSTRAINT")
            || upper.starts_with("PRIMARY KEY")
            || upper.starts_with("UNIQUE")
            || upper.starts_with("FOREIGN KEY")
            || upper.starts_with("CHECK")
            || upper.starts_with("EXCLUDE")
        {
            continue;
        }

        let (name, rest) = split_identifier_and_rest(trimmed)?;
        let column_name = normalize_identifier(name);
        let pg_type = extract_pg_type(rest);
        columns.push(PgColumn {
            name: column_name,
            pg_type,
        });
    }

    if columns.is_empty() {
        return Err(format!(
            "CREATE TABLE {table_name} has no importable columns"
        ));
    }

    Ok(PgTable {
        name: table_name,
        columns,
    })
}

fn parse_copy_header(statement: &str) -> Result<(String, Vec<String>), String> {
    let trimmed = statement.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_ascii_uppercase();
    let from_idx = upper
        .find(" FROM STDIN")
        .ok_or("COPY header missing FROM STDIN")?;
    let target = trimmed["COPY".len()..from_idx].trim();
    let target = strip_optional_keyword(target, "ONLY");
    let open = target.find('(').ok_or("COPY header missing column list")?;
    let close = target
        .rfind(')')
        .ok_or("COPY header missing closing parenthesis")?;
    let table_name = normalize_qualified_ident(target[..open].trim());
    let columns = split_top_level_commas(&target[open + 1..close])
        .into_iter()
        .map(|column| normalize_identifier(column.trim()))
        .collect();
    Ok((table_name, columns))
}

fn translate_copy_block(
    table_name: &str,
    columns: &[String],
    table: Option<&PgTable>,
    rows: &[String],
) -> Result<Vec<String>, String> {
    let schema_columns = schema_columns_for(columns, table);
    let column_list = columns
        .iter()
        .map(|column| quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ");

    let mut inserts = Vec::new();
    for chunk in rows.chunks(256) {
        let mut value_groups = Vec::with_capacity(chunk.len());
        for row in chunk {
            let fields: Vec<&str> = row.split('\t').collect();
            if fields.len() != columns.len() {
                return Err(format!(
                    "COPY row for table '{table_name}' has {} fields but {} columns were declared",
                    fields.len(),
                    columns.len()
                ));
            }

            let values = fields
                .iter()
                .enumerate()
                .map(|(index, field)| copy_field_to_sql(field, &schema_columns[index].pg_type))
                .collect::<Result<Vec<_>, _>>()?;
            value_groups.push(format!("({})", values.join(", ")));
        }

        inserts.push(format!(
            "INSERT INTO {} ({}) VALUES {}",
            quote_ident(table_name),
            column_list,
            value_groups.join(", ")
        ));
    }

    Ok(inserts)
}

fn translate_insert_with_table(
    sql: &str,
    fallback_columns: &[String],
    table: Option<&PgTable>,
) -> Result<String, String> {
    let parts = parse_insert_parts(sql)?;
    let columns = if parts.columns.is_empty() {
        if fallback_columns.is_empty() {
            return Err(format!(
                "INSERT for table '{}' is missing a column list and schema metadata",
                parts.table_name
            ));
        }
        fallback_columns.to_vec()
    } else {
        parts.columns
    };

    let schema_columns = schema_columns_for(&columns, table);
    let groups = parse_value_groups(parts.values_part)?;
    let mut rewritten_groups = Vec::with_capacity(groups.len());

    for group in groups {
        let values = split_top_level_commas(&group);
        if values.len() != columns.len() {
            return Err(format!(
                "INSERT for table '{}' has {} values but {} columns",
                parts.table_name,
                values.len(),
                columns.len()
            ));
        }

        let rewritten = values
            .iter()
            .enumerate()
            .map(|(index, value)| value_to_sql(value, &schema_columns[index].pg_type))
            .collect::<Result<Vec<_>, _>>()?;
        rewritten_groups.push(format!("({})", rewritten.join(", ")));
    }

    let column_list = columns
        .iter()
        .map(|column| quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ");

    Ok(format!(
        "INSERT INTO {} ({}) VALUES {}",
        quote_ident(&parts.table_name),
        column_list,
        rewritten_groups.join(", ")
    ))
}

struct InsertParts<'a> {
    table_name: String,
    columns: Vec<String>,
    values_part: &'a str,
}

fn parse_insert_parts(sql: &str) -> Result<InsertParts<'_>, String> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_ascii_uppercase();
    let insert_prefix = "INSERT INTO";
    if !upper.starts_with(insert_prefix) {
        return Err("statement is not an INSERT".to_string());
    }

    let mut rest = trimmed[insert_prefix.len()..].trim_start();
    if rest.to_ascii_uppercase().starts_with("ONLY ") {
        rest = rest[5..].trim_start();
    }

    let table_end = find_identifier_end(rest);
    let table_name = normalize_qualified_ident(rest[..table_end].trim());
    rest = rest[table_end..].trim_start();

    let columns = if rest.starts_with('(') {
        let close = find_matching_paren(rest, 0)?;
        let parsed = split_top_level_commas(&rest[1..close])
            .into_iter()
            .map(|column| normalize_identifier(column.trim()))
            .collect::<Vec<_>>();
        rest = rest[close + 1..].trim_start();
        parsed
    } else {
        Vec::new()
    };

    let upper_rest = rest.to_ascii_uppercase();
    let values_idx = upper_rest
        .find("VALUES")
        .ok_or("INSERT missing VALUES clause")?;
    let mut values_part = rest[values_idx + "VALUES".len()..].trim();
    if let Some(conflict_idx) = find_top_level_keyword(values_part, "ON CONFLICT") {
        values_part = values_part[..conflict_idx].trim();
    }

    Ok(InsertParts {
        table_name,
        columns,
        values_part,
    })
}

fn parse_value_groups(values_part: &str) -> Result<Vec<String>, String> {
    let mut groups = Vec::new();
    let mut start = None;
    let mut depth = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let trimmed = values_part.trim().trim_end_matches(';');
    let mut chars = trimmed.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        match ch {
            '\'' if !in_double => {
                if in_single {
                    if matches!(chars.peek(), Some((_, '\''))) {
                        chars.next();
                    } else {
                        in_single = false;
                    }
                } else {
                    in_single = true;
                }
            }
            '"' if !in_single => {
                if in_double {
                    if matches!(chars.peek(), Some((_, '"'))) {
                        chars.next();
                    } else {
                        in_double = false;
                    }
                } else {
                    in_double = true;
                }
            }
            '(' if !in_single && !in_double => {
                if depth == 0 {
                    start = Some(idx + ch.len_utf8());
                }
                depth += 1;
            }
            ')' if !in_single && !in_double => {
                if depth == 0 {
                    return Err("unbalanced INSERT parentheses".to_string());
                }
                depth -= 1;
                if depth == 0 {
                    let begin = start.ok_or("malformed INSERT group")?;
                    groups.push(trimmed[begin..idx].trim().to_string());
                }
            }
            _ => {}
        }
    }

    if depth != 0 {
        return Err("unterminated INSERT VALUES group".to_string());
    }
    if groups.is_empty() {
        return Err("INSERT has no VALUES groups".to_string());
    }
    Ok(groups)
}

fn split_top_level_commas(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = input.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        match ch {
            '\'' if !in_double => {
                if in_single {
                    if matches!(chars.peek(), Some((_, '\''))) {
                        chars.next();
                    } else {
                        in_single = false;
                    }
                } else {
                    in_single = true;
                }
            }
            '"' if !in_single => {
                if in_double {
                    if matches!(chars.peek(), Some((_, '"'))) {
                        chars.next();
                    } else {
                        in_double = false;
                    }
                } else {
                    in_double = true;
                }
            }
            '(' if !in_single && !in_double => depth += 1,
            ')' if !in_single && !in_double && depth > 0 => depth -= 1,
            ',' if !in_single && !in_double && depth == 0 => {
                parts.push(input[start..idx].trim().to_string());
                start = idx + 1;
            }
            _ => {}
        }
    }

    let tail = input[start..].trim();
    if !tail.is_empty() {
        parts.push(tail.to_string());
    }
    parts
}

fn split_identifier_and_rest(definition: &str) -> Result<(&str, &str), String> {
    let trimmed = definition.trim();
    if trimmed.is_empty() {
        return Err("empty column definition".to_string());
    }

    if let Some(rest) = trimmed.strip_prefix('"') {
        let close = rest.find('"').ok_or("unterminated quoted identifier")? + 1;
        Ok((&trimmed[..=close], trimmed[close + 1..].trim_start()))
    } else {
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        Ok((&trimmed[..end], trimmed[end..].trim_start()))
    }
}

fn extract_pg_type(rest: &str) -> String {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return "TEXT".to_string();
    }

    let upper = trimmed.to_ascii_uppercase();
    let mut end = trimmed.len();
    for keyword in [
        " DEFAULT ",
        " NOT NULL",
        " NULL",
        " CONSTRAINT ",
        " PRIMARY KEY",
        " REFERENCES ",
        " CHECK ",
        " COLLATE ",
        " GENERATED ",
        " UNIQUE",
    ] {
        if let Some(idx) = upper.find(keyword) {
            end = end.min(idx);
        }
    }

    trimmed[..end].trim().replace("pg_catalog.", "")
}

fn strip_optional_keyword<'a>(input: &'a str, keyword: &str) -> &'a str {
    let upper = input.to_ascii_uppercase();
    if upper.starts_with(keyword) {
        input[keyword.len()..].trim_start()
    } else {
        input
    }
}

fn normalize_qualified_ident(input: &str) -> String {
    split_qualified_ident(input)
        .pop()
        .map(|part| normalize_identifier(&part))
        .unwrap_or_default()
}

fn normalize_identifier(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        trimmed[1..trimmed.len() - 1].replace("\"\"", "\"")
    } else {
        trimmed.to_string()
    }
}

fn split_qualified_ident(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut in_double = false;
    let mut chars = input.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        match ch {
            '"' => {
                if in_double {
                    if matches!(chars.peek(), Some((_, '"'))) {
                        chars.next();
                    } else {
                        in_double = false;
                    }
                } else {
                    in_double = true;
                }
            }
            '.' if !in_double => {
                parts.push(input[start..idx].trim().to_string());
                start = idx + 1;
            }
            _ => {}
        }
    }

    parts.push(input[start..].trim().to_string());
    parts
}

fn quote_ident(input: &str) -> String {
    let first = input.chars().next();
    let simple = first.is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && input
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if simple {
        input.to_string()
    } else {
        format!("\"{}\"", input.replace('"', "\"\""))
    }
}

fn find_identifier_end(input: &str) -> usize {
    let mut in_double = false;
    let mut chars = input.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        match ch {
            '"' => {
                if in_double {
                    if matches!(chars.peek(), Some((_, '"'))) {
                        chars.next();
                    } else {
                        in_double = false;
                    }
                } else {
                    in_double = true;
                }
            }
            c if !in_double && (c.is_whitespace() || c == '(') => return idx,
            _ => {}
        }
    }

    input.len()
}

fn find_matching_paren(input: &str, start: usize) -> Result<usize, String> {
    let mut depth = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = input.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        if idx < start {
            continue;
        }

        match ch {
            '\'' if !in_double => {
                if in_single {
                    if matches!(chars.peek(), Some((_, '\''))) {
                        chars.next();
                    } else {
                        in_single = false;
                    }
                } else {
                    in_single = true;
                }
            }
            '"' if !in_single => {
                if in_double {
                    if matches!(chars.peek(), Some((_, '"'))) {
                        chars.next();
                    } else {
                        in_double = false;
                    }
                } else {
                    in_double = true;
                }
            }
            '(' if !in_single && !in_double => depth += 1,
            ')' if !in_single && !in_double => {
                if depth == 0 {
                    return Err("unbalanced parentheses".to_string());
                }
                depth -= 1;
                if depth == 0 {
                    return Ok(idx);
                }
            }
            _ => {}
        }
    }

    Err("unterminated parenthesized section".to_string())
}

fn find_top_level_keyword(input: &str, keyword: &str) -> Option<usize> {
    let keyword_upper = keyword.to_ascii_uppercase();
    let input_upper = input.to_ascii_uppercase();
    let bytes = input.as_bytes();
    let keyword_len = keyword.len();
    let mut in_single = false;
    let mut in_double = false;
    let mut depth = 0usize;
    let mut idx = 0usize;

    while idx + keyword_len <= input.len() {
        let ch = bytes[idx] as char;
        match ch {
            '\'' if !in_double => {
                if in_single && idx + 1 < input.len() && bytes[idx + 1] as char == '\'' {
                    idx += 2;
                    continue;
                }
                in_single = !in_single;
            }
            '"' if !in_single => {
                if in_double && idx + 1 < input.len() && bytes[idx + 1] as char == '"' {
                    idx += 2;
                    continue;
                }
                in_double = !in_double;
            }
            '(' if !in_single && !in_double => depth += 1,
            ')' if !in_single && !in_double && depth > 0 => depth -= 1,
            _ => {}
        }

        if !in_single && !in_double && depth == 0 && input_upper[idx..].starts_with(&keyword_upper)
        {
            return Some(idx);
        }

        idx += 1;
    }

    None
}

fn schema_columns_for(columns: &[String], table: Option<&PgTable>) -> Vec<PgColumn> {
    columns
        .iter()
        .map(|name| {
            table
                .and_then(|table| {
                    table
                        .columns
                        .iter()
                        .find(|column| column.name.eq_ignore_ascii_case(name))
                })
                .cloned()
                .unwrap_or_else(|| PgColumn {
                    name: name.clone(),
                    pg_type: "TEXT".to_string(),
                })
        })
        .collect()
}

fn pg_col_type(pg_type: &str) -> ColType {
    let upper = pg_type.to_ascii_uppercase().replace("PG_CATALOG.", "");
    if upper.contains('[') || upper.contains("ARRAY") {
        return ColType::Text;
    }
    if upper.contains("BOOL") {
        ColType::Integer
    } else if upper.contains("INT") || upper.contains("SERIAL") {
        ColType::Integer
    } else if upper.contains("NUMERIC") || upper.contains("DECIMAL") {
        ColType::Numeric
    } else if upper.contains("REAL") || upper.contains("DOUBLE") || upper.contains("FLOAT") {
        ColType::Float8
    } else {
        ColType::Text
    }
}

fn qm_type_name(pg_type: &str) -> &'static str {
    match pg_col_type(pg_type) {
        ColType::Integer => "INTEGER",
        ColType::Float8 => "REAL",
        ColType::Text => "TEXT",
        ColType::Boolean => "BOOLEAN",
        ColType::Timestamp => "TIMESTAMP",
        ColType::Date => "DATE",
        ColType::Interval => "INTERVAL",
        ColType::Json => "JSON",
        ColType::Jsonb => "JSONB",
        ColType::Bytea => "BYTEA",
        ColType::Uuid => "UUID",
        ColType::Array => "TEXT[]",
        ColType::Numeric => "NUMERIC",
        ColType::Vector(_) => "VECTOR",
    }
}

fn value_to_sql(value: &str, pg_type: &str) -> Result<String, String> {
    let token = strip_pg_cast(value.trim());
    if token.eq_ignore_ascii_case("NULL") || token.eq_ignore_ascii_case("DEFAULT") {
        return Ok("NULL".to_string());
    }

    match pg_col_type(pg_type) {
        ColType::Integer => normalize_integer_literal(token),
        ColType::Float8 | ColType::Numeric => normalize_float_literal(token),
        ColType::Text => normalize_text_literal(token),
        ColType::Boolean => {
            if token.eq_ignore_ascii_case("true") || token == "t" {
                Ok("TRUE".to_string())
            } else {
                Ok("FALSE".to_string())
            }
        }
        ColType::Timestamp
        | ColType::Date
        | ColType::Interval
        | ColType::Json
        | ColType::Jsonb
        | ColType::Bytea
        | ColType::Uuid
        | ColType::Array
        | ColType::Vector(_) => normalize_text_literal(token),
    }
}

fn copy_field_to_sql(field: &str, pg_type: &str) -> Result<String, String> {
    if field == "\\N" {
        return Ok("NULL".to_string());
    }

    let decoded = decode_copy_field(field);
    match pg_col_type(pg_type) {
        ColType::Integer => normalize_integer_literal(&decoded),
        ColType::Float8 | ColType::Numeric => normalize_float_literal(&decoded),
        ColType::Text => Ok(quote_sql_string(&decoded)),
        ColType::Boolean => {
            if decoded.eq_ignore_ascii_case("true") || decoded == "t" {
                Ok("TRUE".to_string())
            } else {
                Ok("FALSE".to_string())
            }
        }
        ColType::Timestamp
        | ColType::Date
        | ColType::Interval
        | ColType::Json
        | ColType::Jsonb
        | ColType::Bytea
        | ColType::Uuid
        | ColType::Array
        | ColType::Vector(_) => Ok(quote_sql_string(&decoded)),
    }
}

fn normalize_integer_literal(token: &str) -> Result<String, String> {
    let trimmed = token.trim();
    if matches!(trimmed.to_ascii_lowercase().as_str(), "t" | "true") {
        return Ok("1".to_string());
    }
    if matches!(trimmed.to_ascii_lowercase().as_str(), "f" | "false") {
        return Ok("0".to_string());
    }
    if is_quoted_sql_string(trimmed) {
        let inner = unquote_sql_string(trimmed);
        return normalize_integer_literal(&inner);
    }
    trimmed
        .parse::<i64>()
        .map(|value| value.to_string())
        .map_err(|_| format!("invalid integer literal '{trimmed}'"))
}

fn normalize_float_literal(token: &str) -> Result<String, String> {
    let trimmed = token.trim();
    if matches!(trimmed.to_ascii_lowercase().as_str(), "t" | "true") {
        return Ok("1.0".to_string());
    }
    if matches!(trimmed.to_ascii_lowercase().as_str(), "f" | "false") {
        return Ok("0.0".to_string());
    }
    if is_quoted_sql_string(trimmed) {
        let inner = unquote_sql_string(trimmed);
        return normalize_float_literal(&inner);
    }
    trimmed
        .parse::<f64>()
        .map(|_| trimmed.to_string())
        .map_err(|_| format!("invalid floating-point literal '{trimmed}'"))
}

fn normalize_text_literal(token: &str) -> Result<String, String> {
    let trimmed = token.trim();
    if trimmed.eq_ignore_ascii_case("NULL") {
        return Ok("NULL".to_string());
    }
    if let Some(stripped) = trimmed.strip_prefix("E'") {
        let reconstructed = format!("'{}", stripped);
        return Ok(quote_sql_string(&unquote_sql_string(&reconstructed)));
    }
    if is_quoted_sql_string(trimmed) {
        return Ok(quote_sql_string(&unquote_sql_string(trimmed)));
    }
    Ok(quote_sql_string(trimmed))
}

fn strip_pg_cast(token: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let mut depth = 0usize;
    let mut chars = token.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        match ch {
            '\'' if !in_double => {
                if in_single {
                    if matches!(chars.peek(), Some((_, '\''))) {
                        chars.next();
                    } else {
                        in_single = false;
                    }
                } else {
                    in_single = true;
                }
            }
            '"' if !in_single => {
                if in_double {
                    if matches!(chars.peek(), Some((_, '"'))) {
                        chars.next();
                    } else {
                        in_double = false;
                    }
                } else {
                    in_double = true;
                }
            }
            '(' if !in_single && !in_double => depth += 1,
            ')' if !in_single && !in_double && depth > 0 => depth -= 1,
            ':' if !in_single && !in_double && depth == 0 => {
                if matches!(chars.peek(), Some((_, ':'))) {
                    return token[..idx].trim();
                }
            }
            _ => {}
        }
    }

    token.trim()
}

fn decode_copy_field(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn is_quoted_sql_string(token: &str) -> bool {
    token.starts_with('\'') && token.ends_with('\'') && token.len() >= 2
}

fn unquote_sql_string(token: &str) -> String {
    if !is_quoted_sql_string(token) {
        return token.to_string();
    }
    token[1..token.len() - 1].replace("''", "'")
}

fn quote_sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::{count_insert_rows, create_table_sql, parse_pgdump, translate_insert};
    use tempfile::tempdir;

    #[test]
    fn translate_insert_adds_missing_columns() {
        let sql = "INSERT INTO public.users VALUES (1, 'alice', true)";
        let translated = translate_insert(
            sql,
            &["id".to_string(), "name".to_string(), "active".to_string()],
        );
        assert_eq!(
            translated,
            "INSERT INTO users (id, name, active) VALUES (1, 'alice', true)"
        );
        assert_eq!(count_insert_rows(&translated), 1);
    }

    #[test]
    fn parse_pgdump_translates_copy_and_insert() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.sql");
        std::fs::write(
            &path,
            "-- pg_dump sample\n\
             SET statement_timeout = 0;\n\
             CREATE TABLE public.users (\n\
                 id integer,\n\
                 name text,\n\
                 active boolean,\n\
                 score numeric\n\
             );\n\
             COPY public.users (id, name, active, score) FROM stdin;\n\
             1\talice\tt\t10.5\n\
             2\tbob\tf\t7.25\n\
             \\.\n\
             INSERT INTO public.users VALUES (3, 'carol', true, 9.0);\n",
        )
        .unwrap();

        let dump = parse_pgdump(&path).unwrap();
        assert_eq!(dump.tables.len(), 1);
        assert_eq!(
            create_table_sql(&dump.tables[0]),
            "CREATE TABLE users (id INTEGER, name TEXT, active INTEGER, score NUMERIC)"
        );
        assert_eq!(dump.inserts.len(), 2);
        assert!(dump.inserts[0].contains("(1, 'alice', 1, 10.5), (2, 'bob', 0, 7.25)"));
        assert!(dump.inserts[1].contains("(3, 'carol', 1, 9.0)"));
    }
}
