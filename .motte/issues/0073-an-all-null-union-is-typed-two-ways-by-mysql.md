---
id: 73
title: An all-NULL union is typed two ways by MySQL
state: Done
parent: 9
labels: [compat]
created: 2026-08-21T18:26:27Z
updated: 2026-08-26T09:44:53Z
---

## Description

SELECT NULL UNION SELECT NULL
    mysql, prepared   MYSQL_TYPE_NULL
    mysql, executed   MYSQL_TYPE_STRING
    engine, both      MYSQL_TYPE_NULL

Found while adding corpus coverage for #0065. A union materialises, and a materialised column cannot carry the null type, so MySQL's execution path reports a string where its own prepare path reports NULL. `SELECT NULL` on its own is NULL in both.

The engine has one answer for both paths and currently gives NULL, which matches prepare and not execute. Matching execute instead would make `describe` disagree with MySQL's prepare, so neither choice is right — the same shape as the information_schema types in #0050, where a plain scan and a materialised one disagree.

Left out of the differential corpus on purpose: a statement MySQL answers two ways cannot distinguish an engine bug from a MySQL inconsistency. crates/engine/tests/sql.rs pins the engine's behaviour so a change to it is at least deliberate.

## Notes

### 2026-08-26T09:44:53Z — Christopher Vachon (user)

Done in c06c2b7, and the issue's premise was wrong -- measurement is what settled it.

MySQL does not answer this two ways. The result set is binary(0) on both protocols, for the union and every nesting of it. MYSQL_TYPE_NULL appears in exactly one message: COM_STMT_PREPARE's column definitions, and only when the set operation is the statement's own body, because that temporary table is not built until execution. A union inside a derived table, CTE or view is binary(0) at prepare too.

So this was never describe against execute; it was one artefact of MySQL's temp-table timing against one consistent answer the engine got wrong on every path. Giving describe and execute different answers -- which this issue and the charter both seemed to point at -- would have fixed the top-level case and broken the four nested ones.

The rule measured is that materialising gives the column its type. A set operation's result, a derived table, a CTE, a view and a table value constructor read back as a relation all become binary(0); a bare SELECT NULL and a bare VALUES ROW(NULL) stay typeless. CREATE TABLE t AS SELECT NULL UNION SELECT NULL settles what that type is: MySQL declares binary(0), which is MYSQL_TYPE_STRING with charset 63 and length 0. The value stays NULL everywhere -- LENGTH, HEX and INSERT ... SELECT all see NULL -- so the materialised type is observable only as metadata.

Converting inside the merge rather than at the whole-query boundary was wrong and was caught by measuring: MySQL materialises a chain once over all its terms, so bracketing the first pair does not change it, and SELECT NULL UNION SELECT NULL UNION SELECT 1 stays LONGLONG.

Review found a materialisation site the first version missed -- a table value constructor -- and its premise needed correcting too: a bare VALUES ROW(NULL) is MYSQL_TYPE_NULL, not binary(0), so the fix was to stop the constructor falling to VAR_STRING and let it feed the conversion sites already there.

One divergence stays by choice, recorded at three sites with the measurement: the engine answers binary(0) at prepare for an outermost all-NULL set operation where MySQL answers NULL. Reproducing it means encoding 'outermost, at prepare time' -- a permanent exception to describe matching execute, bought for one message. The harness reads result-set metadata only, so it cannot express the divergence either way.

10 mutations, all bite; the test collects mismatches rather than asserting per statement, so a mutation reports every claim it broke. Corpus 1440 -> 1518, baseline empty.

Filed rather than half-fixed: #0125 CTAS makes a zero-column table and desynchronises the connection, #0126 a column's declared length is never sent, #0127 a union of a string and a number ignores MySQL's aggregate string rule.
