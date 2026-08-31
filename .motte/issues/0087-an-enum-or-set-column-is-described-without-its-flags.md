---
id: 87
title: An ENUM or SET column is described without its flags
state: Done
parent: 9
labels: [compat]
created: 2026-08-25T03:02:00Z
updated: 2026-08-31T17:24:08Z
---

## Description

Found by measurement during #0068.

MySQL reports an ENUM column as MYSQL_TYPE_STRING with ENUM_FLAG | NUM_FLAG set and length 1; a SET column as MYSQL_TYPE_STRING with SET_FLAG | NUM_FLAG. This engine gets the type code right, sets no flags at all, and reports length 4096.

Not caught by the differential harness, and that is the point: crates/differential compares only the NOT_NULL and UNSIGNED flags and does not compare column length, so nothing in the corpus can guard a fix. Doing this properly means widening what the harness compares first -- which is worth doing on its own, since the same blind spot hides every other flag -- and then fixing the engine with a case that bites.

Order matters here: widen the harness first and the fix is provable; fix the engine first and the corpus stays green either way, which is how #0081 happened.

## Notes

### 2026-08-28T17:00:29Z — Christopher Vachon (user)

#0126 (a2e3d47) makes the length half of this provable: the harness now compares column_length for any column MySQL reports with a non-empty org_table, and the engine computes an ENUM's width as the widest member x 4 and a SET's as all members plus commas x 4, both measured on 8.3.

The flag half is one line away. ENUM|SET are named in compare.rs's UNMODELLED set, so those bits are excluded from the flags comparison; removing the two names is what turns this issue's fix into something the corpus can prove. Do that first, measure the divergences it reports, then fix.

Note MySQL sets ENUM_FLAG|NUM_FLAG on an ENUM column and SET_FLAG|NUM_FLAG on a SET column -- but NUM is separately excluded and genuinely incomparable (MySQL sets it on a field a temp table produced, so the same column carries it under GROUP BY and not without), so only the ENUM and SET bits themselves are in scope here.

### 2026-08-31T17:24:07Z — Christopher Vachon (user)

Done in 8ead143. Comparison turned on first, per the ordering #0126 established: 50 divergences over 24 statements against the unmodified engine.

Every one was an ENUM. The corpus contained no SET column at all, so half this issue had nothing that could prove it -- a 'a set column' group was added. Turning the comparison on also caught information_schema declaring COLUMN_KEY as char(3) where 8.3 has a real enum('','PRI','UNI','MUL'), which is the only ENUM in that table.

Measured: an ENUM column is MYSQL_TYPE_STRING with ENUM, width widest member x 4; a SET is STRING with SET, width (all members + commas) x 4, both counted in the connection's charset so a latin1 declaration still multiplies by four. A _bin collation adds BINARY. The flag survives a derived table, CTE, view, SELECT *, ORDER BY, GROUP BY, DISTINCT, a generated column and a CTAS copy, and is lost through a UNION -- which falls out for free, because to_wire reads it off declared and the set-operation path already clears that.

NUM verified genuinely incomparable rather than assumed, and with better evidence than #0126 had: within one statement, SELECT few, many, s2 FROM E GROUP BY few, many, s2, the NOT NULL enum has no NUM while the nullable one and the SET do. It follows the temp table, not the type.

MemberList is one value rather than two booleans, so a column carrying both bits is unrepresentable.

Nothing narrowed, nothing blessed. Corpus 1770 -> 1820, baseline empty, both gates re-run independently.

Review also found 64f6ec3 (#0093) three clauses short, fixed here: HAVING and GROUP BY settle only inside the group loop, and UPDATE/DELETE/JOIN ON never resolved their WHERE, so a malformed escape passed silently over an empty table -- and on a write that meant 0 rows affected. Eleven resolve points now share one refusal, so a single mutation neuters them all. The HAVING test that had covered this did not bite: a bare aggregate settles over one implicit group even with no rows, so the old per-row path caught it and the new guard could be deleted with the test still green.
