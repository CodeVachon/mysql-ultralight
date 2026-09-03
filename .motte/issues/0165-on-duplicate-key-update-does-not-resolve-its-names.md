---
id: 165
title: ON DUPLICATE KEY UPDATE does not resolve its names
state: Done
parent: 9
labels: [executor]
created: 2026-09-03T04:51:36Z
updated: 2026-09-03T08:09:40Z
---

## Description

Found while implementing #0102, and the fifth entry point with the identical hole.

    INSERT INTO t (k) VALUES (1) ON DUPLICATE KEY UPDATE k = a.b
    mysql   1054 Unknown column 'a.b' in 'field list'
    engine  inserts the row and reports success

#0102 closed the hole in update(), update_joined(), delete() and delete_joined(). This clause was deliberately left out because its scope is different: it resolves VALUES(x) and the insert's own column list as well as the target table, and #0102's own review found that getting a scope wrong turns a legal statement into a refusal (SET col = DEFAULT parsed as a column named DEFAULT and was refused).

So this needs its scope measured before the guard is added, not copied from the WHERE path. Establish what VALUES(x) resolves against, whether the insert's column list is in scope for the update expressions, and whether an alias introduced by INSERT ... SELECT is visible there.

Recorded in resolve_dml_where's doc comment with the measurement.

## Notes

### 2026-09-03T08:09:39Z — Christopher Vachon (user)

Done in 1333099 (with #0104).

Scope measured before the guard was added, per #0102's warning: the update expressions resolve against the TARGET TABLE'S COLUMNS AND NOTHING ELSE. VALUES(x) names a column of the table rather than a position in the insert's column list, and the insert's list adds nothing to the scope -- it can only name columns the table already has. So unresolved_column needed no special case for VALUES, since it already reads a call's arguments in the surrounding scope.

Two sources have a WIDER scope and are deliberately left unresolved: INSERT ... SELECT, where the clause also sees the SELECT's FROM tables in one flat scope (k = a writes Src.a; k = x.b reads the alias; an output alias and the real name under an alias are both 1054; a name in both is 1052 ambiguous; a set operation contributes nothing), and the row-alias form VALUES (1,1) AS new, which sqlparser parses and nothing here models.

The ordering is the OPPOSITE of the UPDATE path and both are now recorded together: every ODKU TARGET settles before any ODKU VALUE, where resolve_assigned_values deliberately runs above the targets for an UPDATE. Full order measured: 1582 (parser) > 1146 > 1054/3105 on the insert's own column list > 1054 on the written values > 1136 and 3105 for a written value into a generated column > 1054 on every ODKU target > 1054/1210 on the ODKU values interleaved by position > 3105 for a generated ODKU target.

No-collision checklist, which is this issue's version of zero rows: an empty target table, a row present under a different key, a table with NO unique key at all, an untaken CASE branch, and INSERT IGNORE -- all inserted and reported success before, all 1054 now.

Ordering corrections that fell out: INSERT INTO nosuch ... ODKU k = badescape moved 1210 -> 1146; ODKU k = zz, s = badescape moved 1210 -> 1054; INSERT INTO O (id,k) VALUES (1) ODKU zz = 1 moved 1054 -> 1136. A 1582-only pre-pass was kept ahead of the table lookup, because 8.3 raises it in the parser.

Nothing legal now refuses -- confirmed case by case: k = s, VALUES(s), VALUES(k), VALUES(T.k), T.k, db.T.k, back-quoted spellings, k = DEFAULT, TIMESTAMPDIFF, a user variable, a scalar subquery, and both wider-scope forms.

7 mutations, all bite.

This agent also caught that crates/differential/README.md's statement count was stale at HEAD -- it read 2938 where the corpus held 3072, because two parallel merges had each written a count derived from a different base and the coordinator's update pattern silently failed to match. Corrected and re-derived from GROUPS rather than estimated.

Adjacent findings not fixed: written-value names are still not resolved at resolve time (#0102's half -- 8.3 names the value where the engine names the ODKU target); ODKU g = 1 into a generated column is 3105 at resolve on 8.3 and accepted here; and no_such_table omits the database from its message.
