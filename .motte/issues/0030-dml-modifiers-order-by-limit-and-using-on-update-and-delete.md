---
id: 30
title: "DML modifiers: ORDER BY, LIMIT and USING on UPDATE and DELETE"
state: Todo
parent: 6
labels: [executor]
created: 2026-08-18T21:18:33Z
updated: 2026-08-18T21:18:33Z
---

## Description

Parsed and refused today with a clear 1235, because dropping them silently turns a one-row statement into a whole-table one: `DELETE FROM t LIMIT 1` emptied the table before this was caught.

Needed: ORDER BY and LIMIT on single-table UPDATE and DELETE, and DELETE ... USING. None appear in the 605 migrations or the provisioning dumps, which is why they are refused rather than implemented, but they are ordinary MySQL and a test suite may well use them.
