---
id: 112
title: Arithmetic with a NULL literal is a DOUBLE in MySQL
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T23:13:03Z
updated: 2026-08-25T23:13:03Z
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
