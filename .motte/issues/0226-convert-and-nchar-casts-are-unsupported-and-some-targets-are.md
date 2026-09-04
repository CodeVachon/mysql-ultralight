---
id: 226
title: CONVERT and NCHAR casts are unsupported, and some targets are over-accepted
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T04:08:09Z
updated: 2026-09-04T04:08:09Z
---

## Description

Found while implementing #0215. Two directions in one area.

Refused here, answered there:
    CAST('abcd' AS NCHAR(3))                   mysql 'abc' + Warning 3720   engine 1235
    CAST(x AS CHAR(3) CHARACTER SET utf8mb4)   mysql 'abc'                  engine 1064 (sqlparser)
    CONVERT(x, CHAR(3)) / CONVERT(x, BINARY(3)) mysql 'abc' / 616263        engine 1235

Accepted here, refused there -- 1064 on 8.3 for every one:
    CAST(x AS VARCHAR(n)), AS TEXT, AS VARBINARY(n), AS BLOB, AS CHARACTER VARYING(n)

The second list is the same class as #0185 and #0214: syntax the engine takes that a real server rejects, so a query passes here and fails in production.

NCHAR arrives from sqlparser as DataType::Custom("NCHAR", ["3"]), so it needs a Custom arm rather than a new parser.
