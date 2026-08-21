---
id: 59
title: A string literal column is named with its quotes
state: Todo
parent: 9
labels: [compat]
created: 2026-08-21T02:24:55Z
updated: 2026-08-21T13:37:06Z
---

## Description

SELECT 'abc' is named `abc` by MySQL and `'abc'` here. MySQL names a bare string-literal column by its *contents*, where every other unaliased expression is named by its source text — SELECT 1!=2 really is named 1!=2.

Pre-existing, and #0043 did not change it: the source text and the rendered tree agree here, and both keep the quotes.

Adjacent literal concatenation is worth measuring before fixing: MySQL names SELECT 'a' 'b' just `a`, not `ab` and not `'a' 'b'`.

## Notes

### 2026-08-21T13:36:26Z — Christopher Vachon (user)

Related, same area: MySQL names  after the operand, , where this engine uses the written text  — unary plus is a no-op it does not record. Measured on 8.3 while fixing #0063.

### 2026-08-21T13:37:06Z — Christopher Vachon (user)

Related, same area. MySQL names SELECT +s after the operand -- "s" -- where this engine uses the text as written, "+s". Unary plus is a no-op MySQL does not record in the name. Measured on 8.3 while fixing #0063.
