---
id: 35
title: A view's columns are described with a widened type
state: Done
parent: 6
labels: [compat]
created: 2026-08-19T14:44:16Z
updated: 2026-08-20T02:10:20Z
---

## Description

information_schema describes every column of a view, which is what introspection needs — prisma db pull now reads the real 278-table schema. But the type it reports is the wire type rather than the declared one: a view over an INT column reads back with precision 19 rather than 10, and one over VARCHAR(40) reads back as text rather than varchar(40).

The cause is the boundary between a result and a relation. OutColumn carries a wire ColumnType, and materialise_view rebuilds an SqlType from it, so the width and the exact integer size are gone by the time the view's shape is stored. plan_expr already keeps a bare column reference's declared type for the column metadata it emits, so the information exists — it just has nowhere to travel.

The fix is for OutColumn to carry the SqlType it came from rather than only the wire type. That reaches result.rs, exec.rs's projection, and everything that builds an OutColumn.

Found by prisma db pull against a schema with a view.

## Notes

### 2026-08-20T02:10:20Z — Christopher Vachon (user)

Landed. OutColumn carries declared: Option<SqlType> alongside its wire type, and the four sites that rebuilt an SqlType from the wire type go through one sql_type() accessor that uses the declared type where there is one and falls back to the old widening where there is not. Option rather than always-a-type because a computed expression genuinely has none - forcing one would be inventing information. The wire type is unchanged, so nothing a client decodes moved.

A view over a table is now described identically to the table, field for field, and that equality is the test. Previously INT read back as precision 19 and VARCHAR(40) as text.

Four more things came out of it.

- information_schema had two divergent copies of the NUMERIC_SCALE rule: the table loop omitted TypeKind::Bool, the view loop included it, so a BOOLEAN reported scale 0 through a view and nothing through the table it came from. MySQL says 0. Unified into one numeric_scale helper, since two copies of a rule is why they drifted.
- The grouped path built every column with OutColumn::new, so SELECT id, name FROM t GROUP BY id, name described itself differently from SELECT id, name FROM t. A grouped column is still a column; it keeps its type now, through a shared direct_column helper that computed_column_type also uses so the two cannot disagree again.
- A UNION kept the left branch's declared type wholesale, which was a regression this change introduced: SMALLINT UNION BIGINT reported smallint precision 5 while returning bigint values, where the old widening was correct. The type is kept only where both branches agree; otherwise it drops back to the widening. MySQL aggregates the two into the wider type, and this is the honest version of that without a type lattice. My own test had asserted the left branch's type and called it 'as MySQL does', which was wrong.
- A bare DOUBLE reported scale 0; MySQL answers NULL, because an approximate type has no fixed number of decimal places. Pre-existing in both copies.

Also fixed: the widening fallback spelled varchar(512) while carrying length: None, so COLUMN_TYPE said varchar(512) and CHARACTER_MAXIMUM_LENGTH said 65535 - two widths for one column.

And the staleness the precision made visible: a view's shape is worked out once and stored, so widening a column left every view over it describing the old width. MySQL re-resolves a view when it is opened. ALTER TABLE now re-resolves the views in that database afterwards - after, because resolving runs a query and the DDL holds the write lock - and it is cheap because describing reads the schema and not the rows. Best effort by design: a view that no longer resolves keeps the shape it had, because an ALTER must not fail on account of a view it did not touch. ALTER TABLE is the only statement that can change a column's type while leaving the views over it valid; dropping the table or the column makes them fail on use, which is visible rather than silent.

Still open and recorded in docs/prototype.md: a computed view column reports varchar(512), a width that reflects the widening rather than the expression. DOUBLE(M,D) also loses its precision in type parsing, which is separate and pre-existing.

604 migrations still apply, db pull on the real corpus is byte-identical to before, conformance unchanged at 99.9 percent.
