---
id: 125
title: CREATE TABLE AS SELECT makes a zero-column table and desynchronises the connection
state: In Progress
parent: 9
labels: [executor]
created: 2026-08-26T08:49:09Z
updated: 2026-08-27T18:28:26Z
---

## Description

Found while measuring for #0073, entirely pre-existing and unrelated to it.

crates/engine/src/ddl.rs has no CTAS handling: it ignores the query and creates a table with no columns.

    CREATE TABLE q1 AS SELECT 1 AS a;
    SELECT * FROM q1
    -> can't parse: buf doesn't have enough data
    -> Packets out of sync

That is the failure CLAUDE.md singles out as the reason protocol correctness gets its own tests: a desynchronised connection fails far from its cause, and the second statement is where it surfaces rather than the first. The first statement reports success.

No corpus case reaches it, which is why the harness has never seen it.

MySQL derives the column list from the query's result metadata, so the shape is already available -- describe answers exactly that. Worth checking CREATE TABLE ... LIKE and CREATE TEMPORARY TABLE ... AS SELECT in the same pass, and what MySQL declares for a computed column, which is measured in #0073's note: SELECT NULL gives binary(0).
