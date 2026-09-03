---
id: 146
title: BIN over a stored binary column refuses where MySQL answers
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-01T02:16:09Z
updated: 2026-09-03T14:20:29Z
---

## Description

Found by review during #0130, and a deliberate gap rather than a defect -- recording it so the reason is not lost.

    BIN(bl) over a BLOB holding '34xyz'   mysql 100010   engine 1235
    BIN(t)  over a TEXT holding the same  both 100010

#0130 refused BIN over any Value::Bytes because CONV special-cases a hex or bit literal and takes its number (BIN(X'C3A9') is 1100001110101001) while the same bytes in a column are parsed as digits (0) -- and both are Value::Bytes in this engine, so neither answer could be given honestly.

The ambiguity is real for a literal and not for a stored column, which has exactly one right answer. So the refusal is wider than it needs to be, and the narrower fix is available today: refuse only where the value came from a literal.

Closing #0141 -- a hex or bit literal is not read as a number -- removes the ambiguity entirely and lets both cases answer. Either route works; this issue exists so the corpus's deliberate omission of BIN(b) has a reason attached to it.

## Notes

### 2026-09-03T14:02:15Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed exactly. BIN(bl) over a BLOB is 1235 'does not yet support BIN() over a binary string' here and 100010 on 8.3; BIN(t) over TEXT agrees at 100010.
