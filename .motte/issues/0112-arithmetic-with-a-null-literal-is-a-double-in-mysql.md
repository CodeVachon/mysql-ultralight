---
id: 112
title: Arithmetic with a NULL literal is a DOUBLE in MySQL
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T23:13:03Z
updated: 2026-09-03T14:00:24Z
---

## Description

A NULL *literal* anywhere in an arithmetic expression makes MySQL type the whole thing DOUBLE, whatever sat on the other side. Measured on 8.3, over a DECIMAL(10,2) `two`:

    SELECT 1 + NULL       DOUBLE
    SELECT two + NULL     DOUBLE
    SELECT -NULL          DOUBLE
    SELECT NULL / 2       DOUBLE
    SELECT NULL MOD 2     DOUBLE
    SELECT NULL DIV 2     LONGLONG

This engine types each of them by the other operand, so `two + NULL` is NEWDECIMAL and `-NULL` is MYSQL_TYPE_NULL. Every value is NULL either way, so the rows agree and only the type code differs — which the binary protocol does not care about for a NULL, since the null bitmap carries it.

`DIV` is the exception and stays LONGLONG, which is the same shape as the rest of `DIV`'s typing: it answers a BIGINT whatever it divided.

A NULL *column* does not do this: `two + n` over a nullable INT is NEWDECIMAL on both. It is the literal that carries the rule.

Found while adding the decimal arithmetic group to the differential corpus for #0070 — the group covers NULL through columns instead, and says why.

## Notes

### 2026-09-03T14:00:24Z — Christopher Vachon (user)

VALIDATED 2026-09-03 with --column-type-info against mysql:8.3. MySQL's side is exactly as documented. The engine's side has SHRUNK from five divergences to two.

    expression     mysql        engine        verdict
    1 + NULL       DOUBLE       DOUBLE        agrees now
    two + NULL     DOUBLE       NEWDECIMAL    DIVERGES
    -NULL          DOUBLE       DOUBLE        agrees now
    NULL / 2       DOUBLE       NEWDECIMAL    DIVERGES
    NULL MOD 2     DOUBLE       DOUBLE        agrees now
    NULL DIV 2     LONGLONG     LONGLONG      agrees (as documented)
    two + n        NEWDECIMAL   NEWDECIMAL    agrees (the column case, as documented)

Remaining scope: only 'two + NULL' and 'NULL / 2'. The '-NULL is MYSQL_TYPE_NULL' claim in the description is stale.
