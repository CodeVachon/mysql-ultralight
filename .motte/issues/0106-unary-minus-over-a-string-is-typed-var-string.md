---
id: 106
title: Unary minus over a string is typed VAR_STRING
state: Done
parent: 9
labels: [compat]
created: 2026-08-25T16:39:34Z
updated: 2026-09-03T09:22:20Z
---

## Description

Found by the new naming corpus group in #0059, and unrelated to naming.

    SELECT -'abc'   mysql MYSQL_TYPE_DOUBLE value -0   engine VAR_STRING text -0

The value renders the same either way, so this is the type code and not the answer -- but the binary protocol encodes by that code, so a prepared-statement client reads a string where MySQL sends a double.

The corpus statement was narrowed rather than blessed, with the finding recorded in a comment in cases.rs.

## Notes

### 2026-09-03T09:22:20Z — Christopher Vachon (user)

Done in ef881a8 (with #0105).

Measured: unary minus is LONGLONG over an integer, itself over an exact fraction (NEWDECIMAL stays NEWDECIMAL), and DOUBLE over everything else -- a string, a VARCHAR column, a VARBINARY, an ENUM, a BLOB, a JSON document, a temporal and NULL alike. -'abc' is DOUBLE with value -0; -X'41' is DOUBLE with value -65 on 8.3 and -0 here, which is #0141 rather than this.

Unchanged in a derived table and a UNION (DOUBLE both), and unchanged under sql_mode.

Confirmed while here: unary plus is peeled from the TYPE as well as from the name, which #0117 had already made true -- +'abc' is VAR_STRING and named abc.

The value renders the same either way, so this was the type code alone -- but the binary protocol encodes by that code, so a prepared-statement client was reading a string where MySQL sends a double.
