---
id: 96
title: SHOW VARIABLES ignores its LIKE pattern
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-08-25T13:13:52Z
updated: 2026-09-03T14:20:26Z
---

## Description

Found during #0057, pre-existing.

crates/engine/src/exec.rs's SHOW VARIABLES arm discards the LIKE pattern entirely and returns the engine's own 41-name subset whatever was asked for. MySQL filters its several hundred variables by the pattern.

That is why #0057 could not add a differential case proving default_week_format appears in SHOW VARIABLES: no SHOW VARIABLES statement can agree with MySQL while the pattern is ignored and the name set differs, so the fix went in with a unit test and a mutation instead of corpus coverage.

Two halves worth separating when this is taken: honouring the pattern is small and self-contained; matching MySQL's variable set is not, and may not be wanted at all for an engine that deliberately implements a subset. Honouring the pattern over the subset the engine does have is probably the right scope, and would let a corpus case exist for any single variable's presence.

## Notes

### 2026-09-03T14:04:20Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. SHOW VARIABLES LIKE 'sql_mode' returns one row on 8.3 and all 41 of the engine's own variables here. Useful for scoping: the sql_mode VALUE itself already agrees, so honouring the pattern over the subset the engine has -- the scope the description recommends -- would make this exact statement agree, without touching the variable set.
