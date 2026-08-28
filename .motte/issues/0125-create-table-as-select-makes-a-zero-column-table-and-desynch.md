---
id: 125
title: CREATE TABLE AS SELECT makes a zero-column table and desynchronises the connection
state: Done
parent: 9
labels: [executor]
created: 2026-08-26T08:49:09Z
updated: 2026-08-27T22:45:46Z
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

## Notes

### 2026-08-27T22:45:46Z — Christopher Vachon (user)

Done in 219ecad. Two defects wearing one coat, and the protocol half mattered more.

A result set's header is a length-encoded column count, and a payload beginning 0x00 IS an OK packet -- so writing zero does not describe an empty result, it ends the command and leaves the column definitions, rows and terminator to be read as answers to the next statement. There is no encoding for it, so the server refuses before a byte is written. Tested at the wire level over real TCP, asserting both the error and that the next statement still works.

Review caught the backstop causing the desync it prevents: an ERR carries no MORE_RESULTS_EXISTS, so in a multi-statement batch the caller kept writing the remaining result sets and a MULTI_STATEMENTS client read them as answers to its next command. The refusal now ends the batch. The mutation for it was run against BOTH tests to show the batched case is not subsumed by the single-response one -- against the single test it passes, which is the point.

MySQL cannot produce a zero-column result set at all: SELECT; is 1064, SELECT * with no FROM is 1096, CREATE TABLE t () is 1064, dropping a last column is 1090, VALUES ROW() is 3942. Four routes reached it here, three silently -- CREATE TABLE LIKE, CREATE TEMPORARY TABLE AS SELECT, an empty column list, and dropping a last column. All closed.

CTAS declarations measured: type, nullability, DEFAULT and charset kept; AUTO_INCREMENT, primary key and every index dropped. A DEFAULT survives only where the column is read straight out of a stored table -- through a view it falls back to the type's implicit default. Review found it was looked up by RESULT column name, so SELECT id AS name copied a varchar default into an int column and a later insert stored the string; it now uses the source column's own name and requires the types to agree.

The rows go through the same primitives INSERT uses rather than being written straight in. Without that a table was born violating its own CHECK, a PRIMARY KEY took duplicates that INSERT refuses, a NOT NULL column held NULL, and every auto-increment row got 0. All four refusals now match 8.3 (1062, 1048, 3819) including that a refused CTAS leaves no table, which is why the query runs before the name is claimed. LAST_INSERT_ID after a CTAS is the FIRST id generated, not the last.

ALTER TABLE now applies its operations to a copy and swaps it in only when all succeed -- DROP COLUMN a, DROP COLUMN b on a two-column table answered 1090 having already dropped a. That closes the class, not just the guard.

The corpus caught a real bug during development: CREATE TABLE Src LIKE Src answered 1050 where 8.3 answers 1066, checked before anything is looked up.

24 mutations; 23 bite and one passes deliberately to prove the batched case is not subsumed. One was refused by mutate.sh's under-mutation guard, which is what prompted extracting the shared auto-increment helper. Corpus 1612 -> 1684, baseline empty, both gates re-run independently.
