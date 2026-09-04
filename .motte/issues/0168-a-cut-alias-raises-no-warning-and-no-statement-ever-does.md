---
id: 168
title: A cut alias raises no warning, and no statement ever does
state: Done
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T10:09:34Z
updated: 2026-09-04T02:18:15Z
---

## Description

Found while merging #0107, out of its scope.

#0107 established that MySQL cuts a column alias longer than 256 bytes at a character boundary, and that it also raises warning 1466 `Identifier name '%s' is too long` when it does. The cut is implemented and tested (`crates/engine/src/name.rs`, `alias`); the warning is not, and the doc comment on `MAX_ALIAS_BYTES` says 1466 while nothing emits it.

The gap is wider than the alias. The engine has no warning mechanism at all: `OkPacket.warnings` is hard-coded 0 in `crates/protocol/src/packet.rs`, there is no SHOW WARNINGS, and no path anywhere raises one. So this is not a missing line in `alias` -- it is the absence of the thing a line would push onto.

Not a wrong answer under the house rule: the alias itself is cut exactly as MySQL cuts it, and the name reported and the name resolved agree. A client that reads warnings sees a quiet success where MySQL reports one.

Worth scoping as the warning channel generally, not the alias case: once warnings exist, the alias cut, and whatever else MySQL warns about, become one-line emissions. Note the differential harness does not currently compare warning counts either, so it cannot see this -- widening it the way #0126 widened column metadata would be part of the work.

## Notes

### 2026-09-04T02:18:15Z — Christopher Vachon (user)

Landed in 8b42710, gate green, differential 4562/0.

THIS ISSUE'S BODY WAS WRONG ABOUT THE CODE IT NAMES. It records 1466 as 'Identifier name %s is too long'. Measured on 8.3, a cut alias raises ER_REMOVED_SPACES -- 'Leading spaces are removed from name %s' -- and quotes the alias AS WRITTEN AND UNCUT, so a 416-character alias produces a 256-byte column name beside a 416-character message. Implemented as measured, not as filed.

The same code also fires for a leading-space alias, which the engine still does not strip (filed separately).

Note 8.3 raises this at PREPARE (when it names the item), so a prepared long-alias SELECT leaves 8.3's diagnostics area empty while the engine raises at execute. Documented in the corpus, not fixed.
