---
id: 106
title: Unary minus over a string is typed VAR_STRING
state: In Progress
parent: 9
labels: [compat]
created: 2026-08-25T16:39:34Z
updated: 2026-09-03T08:26:12Z
---

## Description

Found by the new naming corpus group in #0059, and unrelated to naming.

    SELECT -'abc'   mysql MYSQL_TYPE_DOUBLE value -0   engine VAR_STRING text -0

The value renders the same either way, so this is the type code and not the answer -- but the binary protocol encodes by that code, so a prepared-statement client reads a string where MySQL sends a double.

The corpus statement was narrowed rather than blessed, with the finding recorded in a comment in cases.rs.
