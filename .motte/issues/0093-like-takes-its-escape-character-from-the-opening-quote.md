---
id: 93
title: LIKE takes its escape character from the opening quote
state: In Progress
parent: 9
labels: [executor]
created: 2026-08-25T12:57:57Z
updated: 2026-08-31T12:21:03Z
---

## Description

Found by review during #0057, and pre-existing -- nothing in that change touches LIKE.

crates/engine/src/expr.rs's Expr::Like arm reads the escape character with

    escape_char.as_ref().and_then(|s| s.to_string().chars().next())

which takes the first character of the *rendered literal*, so it is the opening quote. Every ESCAPE clause therefore sets the escape character to a single quote, whatever was written.

    SELECT 'a%' LIKE 'a!%' ESCAPE '!'     mysql 1   engine 0
    SELECT 'a%' LIKE 'a''%' ESCAPE '!'    mysql 0   engine 1

The second is the decisive one: the engine can only answer 1 there if ' is acting as the escape character.

A wrong answer rather than a refusal, and silent -- LIKE returns a plausible 0 or 1 either way. The fix is to read the string's value rather than its Display rendering. Worth checking NOT LIKE and the same pattern anywhere else a literal is read back through to_string().
