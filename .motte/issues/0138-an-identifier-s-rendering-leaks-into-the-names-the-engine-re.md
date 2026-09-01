---
id: 138
title: An identifier's rendering leaks into the names the engine reports
state: In Progress
parent: 9
labels: [compat]
created: 2026-08-31T13:35:09Z
updated: 2026-09-01T03:03:41Z
---

## Description

Found by the Display-pattern audit in #0093. One class, two visible symptoms.

COLLATE keeps its backticks. crates/engine/src/ddl.rs stores ColumnOption::Collation via to_string() on an ObjectName, and that value is served as information_schema.COLUMNS.COLLATION_NAME and persisted into the state image:

    CREATE TABLE c1 (a VARCHAR(10) COLLATE `utf8mb4_bin`)
    engine reports `utf8mb4_bin` (with backticks)   mysql reports utf8mb4_bin

The expression form at expr.rs already strips them; the DDL form does not. Stale comments at types.rs and tests/image.rs claim COLLATE is not accepted -- it is, and they should be corrected with the fix.

A backtick inside an identifier is doubled and never undone. Nine sites use Display plus trim_matches('`'), which cannot undo the doubling Ident::Display applies:

    CREATE TABLE `we``ird`
    engine information_schema.tables.TABLE_NAME is we``ird   mysql we`ird

Self-consistent for lookups, so the damage is confined to reported names and to what the image stores -- but the image is append-only, so a name written wrong is written wrong permanently.

Sites: exec.rs 3233, 4655, 4682, 7593, 7604, 4978, 7613 and ddl.rs 662, 974. Also exec.rs 771 reads USE's database name out of format!("{use_stmt}") with a dead trim_start_matches("USE ").

The fix for all of them is to read Ident::value rather than its rendering. See #0137 for the INSERT case, which is the same class doing worse damage.
