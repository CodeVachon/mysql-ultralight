---
id: 254
title: A union of two narrow integers is described at BIGINT's width
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-08T14:13:25Z
updated: 2026-09-08T14:13:25Z
---

## Description

Found while checking docs/prototype.md's claims against 8.3 on 2026-09-08, using scripts/ask.sh with COLINFO=1.

The doc's claim that a UNION column "drops back to the widening rather than picking a side" is now only half true, and the remaining half is this.

  CREATE TABLE u (a VARCHAR(10), b VARCHAR(20), s SMALLINT, t TINYINT);

  SELECT a AS c FROM u UNION SELECT b FROM u   -- VAR_STRING len 80, both. AGREE.
  SELECT a AS c FROM u UNION SELECT a FROM u   -- VAR_STRING len 40, both. AGREE.
  SELECT s AS c FROM u UNION SELECT t FROM u   -- SHORT, both.
                                               --   8.3    length 6
                                               --   engine length 20

So the TYPE is aggregated correctly now, including across differing varchar widths, which is what the prototype doc was written before. What is left is the display length on the numeric case: the engine reports 20, which is BIGINT's width, where 8.3 reports 6, which is SMALLINT's.

Distinct from #0127, which is a union mixing a string with a number and falling to VAR_STRING. This one is two integers, and the type is right.

Priority 3: a display width, not a value or a type. Nothing in the target application reads it. Recorded because it was measured, and because prototype.md now states the boundary and this is where the boundary actually is.
