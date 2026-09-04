---
id: 219
title: SHOW VARIABLES ignores its LIKE filter (confirmed again)
state: Cancelled
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T02:00:21Z
updated: 2026-09-04T02:00:32Z
---

## Description

Re-confirmed on 2026-09-03 while implementing #0204: the engine returns all 42 variables where 8.3 returns 1. show_variables in exec.rs takes no filter argument at all.

This duplicates #0096, which is still open and carries the same measurement plus the note that honouring the pattern over the engine's own subset is probably the right scope. Recording the independent re-confirmation here rather than opening a rival issue -- see #0096 for the work.

## Notes

### 2026-09-04T02:00:32Z — Christopher Vachon (user)

Cancelled as a duplicate of #0096, which is still open and carries the same measurement plus scoping notes. The re-confirmation is recorded there instead.
