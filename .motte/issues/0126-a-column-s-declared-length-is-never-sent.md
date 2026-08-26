---
id: 126
title: A column's declared length is never sent
state: Todo
parent: 9
labels: [compat]
created: 2026-08-26T08:49:10Z
updated: 2026-08-26T08:49:10Z
---

## Description

Found by review during #0073, systemic and pre-existing.

MySQL sends column_length from the column's declaration; this engine sends 4096 for every string column and 20 for a plain NULL, whatever was declared. It also never sets the BINARY flag: an all-NULL materialised column is len=0 flags=[BINARY] cs=63 on 8.3, and the engine gets the charset right and the other two wrong.

OutColumn::to_wire never applies a declared length, and ColumnDef::new has no BINARY flag for any type.

Invisible to the differential harness, which compares neither length nor that flag -- the same blind spot recorded in #0087 for the ENUM flags. Order matters for whoever takes this: widen what crates/differential compares first, then fix the engine, or the corpus stays green either way and cannot prove the fix. That is how #0081 happened.
