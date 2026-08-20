---
id: 39
title: A NOT NULL string column is described as nullable
state: Done
parent: 9
labels: [compat]
created: 2026-08-20T14:21:10Z
updated: 2026-08-20T17:50:54Z
---

## Description

Found by the differential harness. ColumnDef::new sets flags::NOT_NULL only for the numeric column types (crates/protocol/src/column.rs:101), and OutColumn::to_wire only ever clears it, via .nullable(). So a nullable INT is right - the flag is set and then cleared - but a NOT NULL VARCHAR never has it set at all and reaches the client as nullable.

Seventeen of the harness's divergences are this. A typed client that trusts the flag models a non-null column as optional, which is the wrong shape for generated code.

The fix belongs in to_wire: set NOT_NULL when the column is not nullable, rather than relying on whatever ColumnDef::new defaulted to.

## Notes

### 2026-08-20T17:50:54Z — Christopher Vachon (user)

Fixed. ColumnDef::nullable() only ever cleared flags::NOT_NULL, and ColumnDef::new only defaulted it on for the numeric codes - so a nullable INT was right by being cleared again while a NOT NULL VARCHAR never had it set. The builder takes a bool now and sets or clears.

Making the flag honest exposed three more, two of them found by review:

- A union is nullable if either branch is, and the merge did not propagate it. Right by accident before, because every string column was nullable anyway.
- ORing that in then regressed a union against a literal: computed columns are unconditionally nullable, so SELECT id FROM N UNION SELECT 1 imported a nullability that does not exist and reported a NOT NULL column as optional - the same failure this issue is about, reached from the other side. Literals now say whether they can be NULL, through a deliberately small never_null: a literal, and arithmetic over things that are themselves never NULL, excluding division because 1/0 is NULL in MySQL.
- RIGHT JOIN relaxed the wrong side. joined_columns was given outer_left || outer_right and only ever relaxed the right relation's columns, but a RIGHT JOIN pads the left. Pre-existing for numeric columns and newly wire-visible for every other type, and the engine was flagging a column NOT NULL and then sending NULL in it.

Also: the test asserted on OutColumn.nullable rather than the wire flag, so it passed with the fix reverted - caught by breaking the fix and watching nothing fail. It reads to_wire().flags now, and names the protocol's own constant rather than copying 0x0001. The prepared-statement placeholder definition says .nullable(true) rather than relying on VarString landing in the arm that sets no flags.

The corpus group grew the three shapes that were diverging and an unmatched row so the outer joins actually pad. Compared against MySQL 8.3 across every declared type, both sides of both outer joins, and five union shapes: identical. Three divergences remain on SELECT id UNION SELECT NULL, which is the type of a NULL literal and belongs to #0040.

604 migrations still apply, db pull byte-identical.
