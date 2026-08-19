---
id: 32
title: Prepare describes by executing, so an expensive query costs twice
state: Todo
parent: 6
labels: [executor]
created: 2026-08-19T02:15:44Z
updated: 2026-08-19T02:15:44Z
---

## Description

COM_STMT_PREPARE has to declare the result columns, because clients cache that metadata and lose every column name without it. `Engine::describe` establishes the shape by running the statement, which means an expensive prepared query is paid for twice: a 20k-row self-join measured 29s to describe and 29s again to execute.

It does not bite on test-sized data, and correctness came first: the declared types must match what execute sends, so describing against a rows-cleared catalog is not safe — computed column types are inferred from the first row.

Worth revisiting with a real planner, or a schema-only relation that carries column types without materialising rows.

Two smaller things found in the same review and deliberately left:
- Prepared column metadata goes stale after DDL. Execute always sends the real definitions, so only a client that caches prepare metadata sees it. Real MySQL raises 1615 'prepared statement needs to be re-prepared'; matching that needs a cheap describe first.
- `SELECT ?` describes its column as `NULL` rather than `?`, because parameters are bound to NULL for the probe. Every other shape describes correctly.
