---
id: 170
title: A second PRIMARY KEY is accepted
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T10:19:28Z
updated: 2026-09-03T10:19:28Z
---

## Description

Found while implementing #0150, and deliberately left out of it.

`CREATE TABLE t (a INT, b INT, PRIMARY KEY (a), PRIMARY KEY (b))` and `ALTER TABLE t ADD PRIMARY KEY (a)` against a table that already has one are both accepted here. MySQL 8.3 refuses both with 1068 (42000) `Multiple primary key defined`.

Measured on mysql:8.3. This is why #0150's duplicate-index-name check leaves the primary key out of the name space it enforces: routing a second PRIMARY through it would answer 1061 where MySQL answers 1068, which is a wrongly numbered refusal rather than a fix.
