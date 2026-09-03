---
id: 166
title: A cross-database multi-table write is refused
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T08:23:54Z
updated: 2026-09-03T14:20:31Z
---

## Description

A deliberate behaviour change introduced by #0153, recorded so the choice is visible rather than buried in a commit.

MySQL executes a multi-table DELETE or UPDATE whose targets live in different databases. This engine commits one database per statement, so rather than writing to whichever database happens to share the table name -- the wrong-answer direction -- it now refuses with 1235.

    DELETE a, b FROM dbone.a JOIN dbtwo.b ON ...
    mysql   deletes from both
    engine  1235

Before #0153 the database part was discarded entirely, so the statement silently wrote to the current schema. The refusal is strictly better than that, and strictly worse than executing it.

Closing this means a statement being able to commit across databases, which is a change to how the engine holds its catalogs rather than to the DELETE path. Worth scoping against the conformance target first: if the Appello schema is single-database throughout -- as its 78 CHARACTER SET clauses and 605 migrations suggest -- recording the limit may be the honest outcome, as it was for #0129.

## Notes

### 2026-09-03T14:05:52Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed exactly. DELETE a, b FROM dbone.a JOIN dbtwo.b is 1235 here ('does not yet support a multi-table DELETE from dbone while probe is the current database') and deletes from both tables on 8.3 -- verified by counting the rows after, 0/0 there against 1/1 here.
