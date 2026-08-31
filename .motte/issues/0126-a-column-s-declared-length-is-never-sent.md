---
id: 126
title: A column's declared length is never sent
state: Done
parent: 9
labels: [compat]
created: 2026-08-26T08:49:10Z
updated: 2026-08-28T17:00:12Z
---

## Description

Found by review during #0073, systemic and pre-existing.

MySQL sends column_length from the column's declaration; this engine sends 4096 for every string column and 20 for a plain NULL, whatever was declared. It also never sets the BINARY flag: an all-NULL materialised column is len=0 flags=[BINARY] cs=63 on 8.3, and the engine gets the charset right and the other two wrong.

OutColumn::to_wire never applies a declared length, and ColumnDef::new has no BINARY flag for any type.

Invisible to the differential harness, which compares neither length nor that flag -- the same blind spot recorded in #0087 for the ENUM flags. Order matters for whoever takes this: widen what crates/differential compares first, then fix the engine, or the corpus stays green either way and cannot prove the fix. That is how #0081 happened.

## Notes

### 2026-08-28T17:00:11Z — Christopher Vachon (user)

Done in a2e3d47. Widening the harness first was the whole point, and it produced the measurement this issue existed for.

Before: name, column_type, and two flag bits (NOT_NULL, UNSIGNED) -- four of the eleven fields a ColumnDefinition41 carries. After: name, type, column_length, the full flags word less three named groups, and whether the column is bytes or characters.

Stage 1 measurement against the unmodified engine: 11382 divergences over 1684 statements, from a corpus that had been reporting an empty baseline. column_length 3388, flags 3138, origin fields 2924, decimals 1148, character_set 784.

Engine fixes: declared width for every type, both BINARY rules, NUM (the engine set it on every numeric column; 8.3 sets it on none), BLOB restricted to a stored column, ZEROFILL on a stored YEAR, and information_schema's own COLUMN_TYPE and _bin collations.

Widths measured: characters x 4 for text, bytes for binary, and a latin1 VARCHAR(10) also reports 40 because the server converts on the way out. TINYINT 4 / UNSIGNED 3, TINYINT(1) 1, INT 11/10, BIGINT 20, FLOAT(M,D) is M, DECIMAL is M + point-if-scale + sign-if-signed. LONGTEXT and JSON saturate at 4294967295.

BINARY has two rules: a declared column is binary for a temporal, blob or document and never for a number; an expression is binary when its collation is. Review caught the first cut applying the declared rule to expressions, putting the flag on GROUP_CONCAT -- the bit Prisma reads to hand back a Buffer and then refuse it as a string. That was a changed wire answer, not a pre-existing one.

That hole was in the widening as well: a computed column was still asked only about two bits. Every column is now also compared on binary-vs-not, which is the half of the charset that is a fact about the column rather than the client. It found 312 more divergences over 90 statements, 268 of them computed JSON documents described as bytes.

Not compared, each with its count and reason in compare.rs and the README: decimals (#0111), the charset id itself, the origin fields, the schema-role flag bits, ENUM/SET bits (#0087 -- two names out of UNMODELLED is how that gets proved), NUM (genuinely incomparable: MySQL sets it on a field a temp table produced, so the same column carries it under GROUP BY and not without one -- comparing it would compare query plans), and computed widths (Item::max_length, unmodelled).

11 statements decline a field via a new Case constructor, each with its reason; nothing blessed into baseline.txt, which is still empty.

FORMAT_VERSION 8 -> 9: binary_collation appended, because CTAS can now put a binary-collated column into a user table, which invalidated the premise for leaving it out.

Provable now: #0087 for the length half, #0111 for a DECIMAL column's declared length. #0107 already was. #0127 is not -- a union column has an empty org_table, so its width is out of scope.

16 mutations, all bite except one doc-comment finding with no runtime behaviour, which says so. Corpus 1684 -> 1692, baseline empty, both gates re-run independently.
