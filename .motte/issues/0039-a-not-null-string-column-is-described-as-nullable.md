---
id: 39
title: A NOT NULL string column is described as nullable
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T14:21:10Z
updated: 2026-08-20T14:21:10Z
---

## Description

Found by the differential harness. ColumnDef::new sets flags::NOT_NULL only for the numeric column types (crates/protocol/src/column.rs:101), and OutColumn::to_wire only ever clears it, via .nullable(). So a nullable INT is right - the flag is set and then cleared - but a NOT NULL VARCHAR never has it set at all and reaches the client as nullable.

Seventeen of the harness's divergences are this. A typed client that trusts the flag models a non-null column as optional, which is the wrong shape for generated code.

The fix belongs in to_wire: set NOT_NULL when the column is not nullable, rather than relying on whatever ColumnDef::new defaulted to.
