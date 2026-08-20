---
id: 48
title: A FLOAT column keeps and prints double precision
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T15:57:59Z
updated: 2026-08-20T15:57:59Z
---

## Description

Split out of #0047. A FLOAT column holds four bytes in MySQL and prints about six significant digits: storing 1.234567890123 and reading it back gives 1.23457. This engine keeps the full double and prints 1.234567890123.

Rounding the stored value to f32 is one line and makes the value right, but it makes the *rendering* worse rather than better - 1.2345678806304932 - because Value::Float does not know the column it came from and renders every double the same way. Getting it right needs the column's type at render time.

Left alone deliberately, which is why FLOAT keeps its MYSQL_TYPE_DOUBLE wire type in #0038: declaring MYSQL_TYPE_FLOAT would make the binary protocol write four bytes for a value the engine stored as eight. FLOAT does not appear in the target schema, which uses DOUBLE and DECIMAL.
