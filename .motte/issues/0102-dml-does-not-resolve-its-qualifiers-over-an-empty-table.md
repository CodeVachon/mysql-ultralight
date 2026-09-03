---
id: 102
title: DML does not resolve its qualifiers over an empty table
state: In Progress
parent: 9
labels: [compat]
created: 2026-08-25T14:48:58Z
updated: 2026-09-03T04:09:47Z
---

## Description

Found while implementing #0058, same root cause and a different code path.

    UPDATE Bare SET k=1 WHERE a.b=1    mysql 1054 at prepare   engine 0 rows affected
    DELETE FROM Bare WHERE a.b=1       mysql 1054 at prepare   engine 0 rows affected
    UPDATE Bare SET k=a.b              mysql 1054 at prepare   engine 0 rows affected

A name is only resolved when a row is evaluated against it, so over zero rows nothing is resolved and the statement succeeds. #0058 fixed that for SELECT by checking at prepare, but its checks are wired into select(), project() and grouped(), none of which UPDATE or DELETE go through.

The mechanism to reuse is unresolved_column and refused_at_prepare in crates/engine/src/exec.rs.

## Notes

### 2026-08-31T17:24:08Z — Christopher Vachon (user)

#0087 (8ead143) put the escape-clause guard at exactly the line this issue's name check belongs on, in update(), update_joined(), delete() and delete_joined(), with a comment saying so -- deliberately without adding unresolved_column, so this issue was not closed silently.

That makes this a one-line change beside an existing guard rather than a re-derivation. The ordering is already measured and encoded there: 8.3 answers 1146 for a missing table before either check, and 1210 for a malformed escape before 1054 for an unknown assignment target.

Confirmed while doing it: UPDATE and DELETE resolve no names at all today. UPDATE LE SET s='x' WHERE nope LIKE 'a' ESCAPE '!!' is 1054 on 8.3 and 0 rows affected here. The consequence of fixing only the escape half is recorded in the code -- a DML statement wrong both ways currently answers 1210 where 8.3 answers 1054, and closing this issue is what corrects that ordering.
