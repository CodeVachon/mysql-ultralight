---
id: 61
title: A SQL boolean beside a document is read as a number
state: Todo
parent: 9
labels: [compat]
created: 2026-08-21T12:40:09Z
updated: 2026-08-21T12:40:09Z
---

## Description

Split out of #0045. MySQL reads the SQL keyword TRUE as the JSON value `true` and the literal 1 as the JSON number 1, so they compare against different documents:

    CAST('true' AS JSON) = TRUE   mysql 1   engine 0
    CAST('true' AS JSON) = 1      mysql 0   engine 0
    CAST('1'    AS JSON) = TRUE   mysql 0   engine 0
    CAST('1'    AS JSON) = 1      mysql 1   engine 1

Only the first row differs, and the engine cannot currently do better: crates/engine/src/value.rs has no boolean Value, so TRUE and 1 both evaluate to Value::Int(1) and are indistinguishable by the time a comparison sees them. sqlparser does keep them apart — Expr::Value(Boolean) versus Number — so the information exists at the expression level and is thrown away below it.

Fixing it means either a Value::Bool variant, which the image format and the wire encoding both have to learn, or converting the operand where the expression is still in hand. The same shape as #0054, where a temporal loses its type on the way into arithmetic.
