//! Locating SQL in a target codebase.
//!
//! Three places, because the earlier survey looked in only one and got the
//! answer wrong: Prisma migrations, the provisioning dumps that build a dev
//! database, and the raw SQL embedded in application TypeScript.

use std::path::{Path, PathBuf};

/// Where a body of SQL came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    Migrations,
    Provisions,
    ApplicationCode,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Migrations => "migrations",
            Origin::Provisions => "provisions",
            Origin::ApplicationCode => "application code",
        }
    }
}

pub struct Source {
    pub origin: Origin,
    pub path: PathBuf,
    pub sql: String,
    /// The line in `path` that `sql` starts on. SQL files start at 1; SQL
    /// recovered from a TypeScript template starts wherever its backtick was,
    /// so reported line numbers point at real source rather than at an offset
    /// into a synthetic blob.
    pub line_offset: usize,
}

/// Recursively collects files whose name matches `want`.
fn collect(dir: &Path, want: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // node_modules dwarfs the source tree and contains no target SQL.
            if path
                .file_name()
                .is_some_and(|n| n == "node_modules" || n == ".git")
            {
                continue;
            }
            collect(&path, want, out);
        } else if want(&path) {
            out.push(path);
        }
    }
}

pub fn load_sql_files(dir: &Path, origin: Origin) -> Vec<Source> {
    let mut paths = Vec::new();
    collect(
        dir,
        &|p| p.extension().is_some_and(|e| e == "sql"),
        &mut paths,
    );
    paths.sort();

    paths
        .into_iter()
        .filter_map(|path| {
            std::fs::read_to_string(&path).ok().map(|sql| Source {
                origin,
                path,
                sql,
                line_offset: 1,
            })
        })
        .collect()
}

/// Extracts SQL from Prisma raw-query tagged templates in TypeScript.
///
/// Interpolations become `?`, which is exactly what Prisma does: `$queryRaw`
/// binds them as parameters rather than splicing them in. That makes the
/// recovered text real, parseable SQL rather than an approximation.
pub fn load_raw_queries(dir: &Path) -> Vec<Source> {
    let mut paths = Vec::new();
    collect(
        dir,
        &|p| p.extension().is_some_and(|e| e == "ts" || e == "tsx"),
        &mut paths,
    );
    paths.sort();

    let mut out = Vec::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // One Source per template rather than one per file: concatenating them
        // would make every reported line number an offset into a blob that does
        // not exist on disk.
        for template in extract_tagged_templates(&text) {
            out.push(Source {
                origin: Origin::ApplicationCode,
                path: path.clone(),
                sql: template.sql,
                line_offset: template.line,
            });
        }
    }
    out
}

/// SQL recovered from one tagged template, with the line it began on.
#[derive(Debug, PartialEq)]
pub struct Template {
    pub sql: String,
    pub line: usize,
}

const TAGS: &[&str] = &["$queryRaw", "$executeRaw", "Prisma.sql"];

/// Finds `$queryRaw` / `$executeRaw` tagged templates and returns their SQL.
fn extract_tagged_templates(text: &str) -> Vec<Template> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    // Newlines consumed so far, so each template reports its real start line.
    let mut line = 1usize;
    let mut counted_to = 0usize;

    while i < bytes.len() {
        // Byte comparison, not string slicing: `i` walks raw bytes and the
        // source contains multi-byte characters, so slicing can land inside one.
        let Some(tag) = TAGS.iter().find(|t| bytes[i..].starts_with(t.as_bytes())) else {
            i += 1;
            continue;
        };

        while counted_to < i {
            if bytes[counted_to] == b'\n' {
                line += 1;
            }
            counted_to += 1;
        }

        // Between the tag and the backtick there may be `Unsafe`, whitespace,
        // and a TypeScript generic argument such as `<{ id: string }[]>`.
        // Generics nest, so they need depth tracking rather than a scan to the
        // first `>`.
        let mut j = i + tag.len();
        loop {
            while j < bytes.len()
                && (bytes[j].is_ascii_alphabetic() || bytes[j].is_ascii_whitespace())
            {
                j += 1;
            }
            if bytes.get(j) == Some(&b'<') {
                let mut depth = 1usize;
                j += 1;
                while j < bytes.len() && depth > 0 {
                    match bytes[j] {
                        b'<' => depth += 1,
                        b'>' => depth -= 1,
                        _ => {}
                    }
                    j += 1;
                }
                continue;
            }
            break;
        }
        if bytes.get(j) != Some(&b'`') {
            i += tag.len();
            continue;
        }

        // The backtick can sit lines below the tag when a generic wraps.
        let open_line = line + bytes[i..j].iter().filter(|b| **b == b'\n').count();

        match read_template(text, j) {
            Some((sql, end)) => {
                if !sql.trim().is_empty() {
                    out.push(Template {
                        sql,
                        line: open_line,
                    });
                }
                i = end;
            }
            None => i += tag.len(),
        }
    }

    out
}

/// Reads a template literal starting at the opening backtick, replacing each
/// `${...}` with a placeholder. Returns the SQL and the index just past the
/// closing backtick.
fn read_template(text: &str, open: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut sql = String::new();
    let mut i = open + 1;

    while i < bytes.len() {
        match bytes[i] {
            // A backslash escape must still contribute a character. Dropping
            // the pair fused the tokens on either side, turning
            // `SELECT a\nFROM t` into `SELECT aFROM t` and reporting it as a
            // parser gap that does not exist.
            b'\\' => {
                i += 1;
                let escaped = text[i..].chars().next()?;
                i += escaped.len_utf8();
                match escaped {
                    'n' => sql.push('\n'),
                    't' => sql.push('\t'),
                    'r' => sql.push('\r'),
                    '0' => sql.push('\0'),
                    '\\' | '`' | '$' | '\'' | '"' => sql.push(escaped),
                    // Not a JavaScript escape, so both characters reach the
                    // SQL. This is how `'%\\_%'` in source arrives at MySQL as
                    // the escape sequence `%\_%`.
                    other => {
                        sql.push('\\');
                        sql.push(other);
                    }
                }
            }
            b'`' => return Some((sql, i + 1)),
            b'$' if bytes.get(i + 1) == Some(&b'{') => {
                // Interpolations nest, so track depth rather than scanning to
                // the first closing brace.
                let mut depth = 1usize;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    match bytes[i] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                sql.push('?');
            }
            _ => {
                let ch = text[i..].chars().next()?;
                sql.push(ch);
                i += ch.len_utf8();
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience for the many tests that only care about the SQL.
    fn sql_of(text: &str) -> Vec<String> {
        extract_tagged_templates(text)
            .into_iter()
            .map(|t| t.sql)
            .collect()
    }

    #[test]
    fn extracts_a_simple_tagged_template() {
        let ts = "const r = await prisma.$queryRaw`SELECT 1 FROM T`;";
        assert_eq!(sql_of(ts), vec!["SELECT 1 FROM T"]);
    }

    #[test]
    fn interpolations_become_placeholders() {
        let ts = "prisma.$queryRaw`SELECT * FROM T WHERE id = ${id} AND x = ${a + b}`";
        assert_eq!(sql_of(ts), vec!["SELECT * FROM T WHERE id = ? AND x = ?"]);
    }

    #[test]
    fn nested_braces_inside_an_interpolation_are_handled() {
        let ts = "prisma.$queryRaw`SELECT ${fn({ a: 1 })} FROM T`";
        assert_eq!(sql_of(ts), vec!["SELECT ? FROM T"]);
    }

    #[test]
    fn a_non_template_call_is_ignored() {
        assert!(sql_of("prisma.$queryRawUnsafe(someString)").is_empty());
    }

    #[test]
    fn handles_a_typescript_generic_between_tag_and_template() {
        let ts = "prisma.$queryRaw<{ id: string }[]>`SELECT id FROM T`";
        assert_eq!(sql_of(ts), vec!["SELECT id FROM T"]);
    }

    #[test]
    fn handles_a_nested_generic() {
        let ts = "prisma.$queryRaw<Array<Record<string, number>>>`SELECT 1`";
        assert_eq!(sql_of(ts), vec!["SELECT 1"]);
    }

    #[test]
    fn finds_several_templates_in_one_file() {
        assert_eq!(
            sql_of("a.$queryRaw`SELECT 1`; b.$executeRaw`DELETE FROM T`;").len(),
            2
        );
    }

    /// Escapes must contribute a character. Dropping the pair fuses the tokens
    /// on either side and invents a parser gap.
    #[test]
    fn escapes_are_decoded_rather_than_dropped() {
        assert_eq!(
            sql_of(r"x.$queryRaw`SELECT a\nFROM t`"),
            vec!["SELECT a\nFROM t"]
        );
        assert_eq!(sql_of(r"x.$queryRaw`SELECT '\t'`"), vec!["SELECT '\t'"]);
    }

    /// A backslash before a non-escape survives, which is how a SQL LIKE
    /// escape written in a template reaches MySQL intact.
    #[test]
    fn a_sql_escape_survives_the_template() {
        assert_eq!(
            sql_of(r"x.$queryRaw`WHERE a LIKE '%\_%'`"),
            vec![r"WHERE a LIKE '%\_%'"]
        );
    }

    #[test]
    fn a_backtick_can_be_escaped_inside_a_template() {
        assert_eq!(
            sql_of(r"x.$queryRaw`SELECT \`col\` FROM t`"),
            vec!["SELECT `col` FROM t"]
        );
    }

    /// Line numbers must point at the real file, not at an offset into a
    /// concatenation of every template in it.
    #[test]
    fn templates_report_the_line_they_start_on() {
        let ts = "import x;\n\nconst a = prisma.$queryRaw`SELECT 1`;\n\n\nconst b = prisma.$queryRaw`SELECT 2`;";
        let found = extract_tagged_templates(ts);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].line, 3);
        assert_eq!(found[1].line, 6);
    }
}
