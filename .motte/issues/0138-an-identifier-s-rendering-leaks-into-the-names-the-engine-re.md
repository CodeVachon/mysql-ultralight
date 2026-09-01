---
id: 138
title: An identifier's rendering leaks into the names the engine reports
state: Done
parent: 9
labels: [compat]
created: 2026-08-31T13:35:09Z
updated: 2026-09-01T07:46:27Z
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

## Notes

### 2026-09-01T07:46:27Z — Christopher Vachon (user)

Done in 294e19c, with #0137.

Nine sites paired Display with trim_matches('`'), which cannot undo the doubling Ident::Display applies, so CREATE TABLE `we``ird` reported the name back with both backticks. Quoting a name back into a message is now an explicit inverse rather than the absence of trimming -- the foreign key errors need it, because 8.3 writes REFERENCES `pa``rent` there: that message is quoting SQL, not reporting a name.

COLLATE was the larger half. Reading it correctly showed it was accepted and then ignored, which is two wrong answers rather than one: under a UNIQUE key over VARCHAR(10) COLLATE utf8mb4_bin, 8.3 keeps 'a' and 'A' apart where the engine raised 1062, and WHERE a = 'A' returned the wrong row. Now refused -- see #0149, which tracks that narrowing.

Review then found the refusal bypassable: only the column-level clause was refused while the table-level one was still parsed and dropped, and this repo's own example migrations write it that way on all five tables. Both spellings now go through one reader, since two readers of one clause is how they came to disagree, and COLLATION_NAME resolves narrowest-first -- column, then table, then database -- which is 8.3's measured order.

Which collation names exist is now measured rather than guessed: 286 read off information_schema.COLLATIONS on stock 8.3, embedded sorted with a test that they stay sorted, because binary_search over an unsorted slice would quietly report a real collation as unknown. A name outside that set is 1273 exactly as MySQL writes it; a real one this engine cannot compare in is the unsupported refusal. The first cut conflated the two and called utf8mb4_bin unknown, which is a false claim about a name MySQL has.

information_schema.COLLATIONS is now built from that same list rather than maintained beside it. A server that advertises a collation and then refuses it is contradicting itself.

Also fixed: COLLATION_NAME ignored binary_collation -- the one collation the engine models -- so a CTAS copy of COLUMN_TYPE carried the BINARY wire flag while reporting utf8mb4_0900_ai_ci; and only char/varchar/text/enum/set report a collation at all, where the engine gave every column one, so an INT claimed utf8mb4_0900_ai_ci. A view now carries its base column's collation.

22 mutations across the change, all bite. One false green was caught in the agent's own work: three mutations 'passed' because a subprocess did not inherit cargo on PATH, so every command failed with command-not-found and mutate.sh read that as the test failing -- the #0081 class from a new direction. A cargo --version assertion now runs before anything is mutated.

Corpus 2030 -> 2080, baseline empty, both gates verified independently.
