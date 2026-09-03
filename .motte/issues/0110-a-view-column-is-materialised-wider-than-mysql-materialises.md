---
id: 110
title: A view column is materialised wider than MySQL materialises it
state: Done
parent: 9
labels: [compat]
created: 2026-08-25T19:32:32Z
updated: 2026-09-03T13:19:21Z
---

## Description

Found by the corpus while implementing #0059, and about a view's shape rather than its names.

    CREATE VIEW LM AS SELECT 1 MOD 3;  SELECT * FROM LM
    mysql   INT
    engine  BIGINT

The corpus case was removed rather than blessed, with the reason recorded in cases.rs, so the baseline stays empty. The same view is still covered by name in the unit test.

Related in kind to #0038, which narrowed the integer type codes so an expression is described the way MySQL describes it. This is the materialised column a view creates from that expression, which is a second place the width is decided and evidently a different rule.

Worth measuring several expressions rather than fixing 1 MOD 3 alone: whether the width follows the operands, the operator, or the range the result can occupy.

## Notes

### 2026-09-03T13:19:21Z — Christopher Vachon (user)

Landed in a0e98a8, gate green. The rule is Item::max_length: <=9 printable characters materialises int, >=10 bigint. Measured, not read -- 99999999 is int and 999999999 is bigint.

The half the issue did not say, and which would have cost the empty baseline if assumed: MySQL only narrows where it MATERIALISES. A derived table or CTE with a FROM is merged into the reading query and keeps LONGLONG and its BINARY flag. So materialised_by_mysql claims only the FROM-less SELECT. DISTINCT/GROUP BY/LIMIT also materialise -- filed as #0189 rather than guessed at.

CTAS narrows by a DIFFERENT rule (mi+1, LENGTH, INSTR, CRC32 are int under CTAS and bigint under a view), so it deliberately does not share this path; that stays #0126.

Review caught a real bug in the change: a UNION view's stored shape was narrowed, declaring int over a column returning 2^63-1. Also two panics on unvalidated display widths, INT(0) and INT(4294967295).

Restored the corpus case this issue had removed rather than blessed, and the two identifier-group cases that had been forced to materialise a string to dodge this exact bug. Divergences filed as #0188, #0189, #0190.
