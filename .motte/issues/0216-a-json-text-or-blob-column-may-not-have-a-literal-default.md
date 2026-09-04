---
id: 216
title: A JSON, TEXT or BLOB column may not have a literal DEFAULT
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T01:36:11Z
updated: 2026-09-04T01:36:11Z
---

## Description

Found while implementing #0211, measured on 8.3.

    CREATE TABLE m3 (k INT PRIMARY KEY, doc JSON DEFAULT '{"a": 1}')
    mysql   1101 (42000) BLOB, TEXT, GEOMETRY or JSON column 'doc' can't have a default value
    engine  accepted

DEFAULT (JSON_OBJECT('a',1)) -- the expression form -- is legal on both and agrees.

The rule covers TEXT and BLOB as well as JSON, so it is a DDL check of its own rather than part of #0211's value-assignment rule, which is why it was left. A migration declaring such a default passes here and fails against a real server.
