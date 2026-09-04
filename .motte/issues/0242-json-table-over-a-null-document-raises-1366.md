---
id: 242
title: JSON_TABLE over a NULL document raises 1366
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-04T08:10:40Z
updated: 2026-09-04T08:10:40Z
---

## Description

Found while implementing #0230 and unrelated to it -- reproduces with a constant ON 1 = 1.

    SELECT jt.v FROM JSON_TABLE(CAST(NULL AS JSON), '$[*]' COLUMNS (v INT PATH '$')) jt
    mysql   no rows
    engine  1366 Incorrect int value for column 'v'

Same for a JSON_TABLE join over an empty table. The engine appears to coerce a missing value into the declared column type rather than producing no row at all.

The target application declares 54 JSON columns but no JSON_TABLE usage was found in the conformance scan, which is why this is priority 3.
