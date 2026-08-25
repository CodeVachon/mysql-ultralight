---
id: 102
title: DML does not resolve its qualifiers over an empty table
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T14:48:58Z
updated: 2026-08-25T14:48:58Z
---

## Description

Found while implementing #0058, same root cause and a different code path.

    UPDATE Bare SET k=1 WHERE a.b=1    mysql 1054 at prepare   engine 0 rows affected
    DELETE FROM Bare WHERE a.b=1       mysql 1054 at prepare   engine 0 rows affected
    UPDATE Bare SET k=a.b              mysql 1054 at prepare   engine 0 rows affected

A name is only resolved when a row is evaluated against it, so over zero rows nothing is resolved and the statement succeeds. #0058 fixed that for SELECT by checking at prepare, but its checks are wired into select(), project() and grouped(), none of which UPDATE or DELETE go through.

The mechanism to reuse is unresolved_column and refused_at_prepare in crates/engine/src/exec.rs.
