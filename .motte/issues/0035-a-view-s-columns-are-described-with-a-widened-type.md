---
id: 35
title: A view's columns are described with a widened type
state: Todo
parent: 6
labels: [compat]
created: 2026-08-19T14:44:16Z
updated: 2026-08-19T14:44:16Z
---

## Description

information_schema describes every column of a view, which is what introspection needs — prisma db pull now reads the real 278-table schema. But the type it reports is the wire type rather than the declared one: a view over an INT column reads back with precision 19 rather than 10, and one over VARCHAR(40) reads back as text rather than varchar(40).

The cause is the boundary between a result and a relation. OutColumn carries a wire ColumnType, and materialise_view rebuilds an SqlType from it, so the width and the exact integer size are gone by the time the view's shape is stored. plan_expr already keeps a bare column reference's declared type for the column metadata it emits, so the information exists — it just has nowhere to travel.

The fix is for OutColumn to carry the SqlType it came from rather than only the wire type. That reaches result.rs, exec.rs's projection, and everything that builds an OutColumn.

Found by prisma db pull against a schema with a view.
