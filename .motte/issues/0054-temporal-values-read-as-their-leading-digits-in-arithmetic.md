---
id: 54
title: Temporal values read as their leading digits in arithmetic
state: Todo
parent: 9
labels: [executor]
created: 2026-08-20T19:38:59Z
updated: 2026-08-20T19:38:59Z
---

## Description

Found by review during #0042. MySQL converts a temporal to its packed YYYYMMDD[HHMMSS] integer in an arithmetic context; this engine reads the leading digits of the text.

    SELECT d + 0   -- DATE 2024-01-05           engine 2024      mysql 20240105
    SELECT dt + 0  -- DATETIME 2024-01-05 06:07:08  engine 2024   mysql 20240105060708
    SELECT tm + 0  -- TIME 06:07:08              engine 6         mysql 60708

Before #0042 these were NULL, because arithmetic answered NULL for anything it could not parse as a whole number. #0042 made a non-numeric operand read as its leading numeric part, which is right for strings and turned this from visibly broken into quietly wrong.

The engine cannot currently tell the two apart. A temporal is stored as Value::Text, and MySQL's answer depends on the operand's *type*, not its text: a DATE column gives 20240105 where the string literal '2024-01-05' gives 2024 — both verified on 8.3. Sniffing the text shape in Value::as_number would fix the column and break the literal, so it is a coin flip rather than a fix.

Doing this properly means the arithmetic path knowing the operand's declared type, either by tagging temporals in Value the way Value::Json already tags documents, or by routing the conversion through the expression's type rather than its value.
