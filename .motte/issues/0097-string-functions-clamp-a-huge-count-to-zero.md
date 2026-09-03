---
id: 97
title: String functions clamp a huge count to zero
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-08-25T13:45:18Z
updated: 2026-09-03T14:20:26Z
---

## Description

Found while fixing a review finding on #0057, which hit the same hazard in session.rs.

crates/engine/src/functions.rs reads a count with as_int().unwrap_or(0).max(0) as usize at five sites -- SPACE, LEFT, RIGHT, LPAD and RPAD. Value::as_int deliberately wraps a u64 above i64::MAX to a negative (documented at value.rs:177), so a literal above that range clamps to 0 rather than to a huge count.

A consequence of 3b6a342 adding Value::Unsigned: before it, a literal that large could not reach these functions as an exact value at all.

Measure what MySQL does before fixing, because the current behaviour may be the kinder one: MySQL presumably tries to build the string and fails on max_allowed_packet or memory, and answering 0 is at least cheap. Whatever it does, the engine should do deliberately rather than as a side effect of a wrap it did not intend -- as_exact_int is the accessor #0057 switched to for the same reason.

## Notes

### 2026-09-03T14:01:17Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3. The premise holds, and the issue's open question -- 'measure what MySQL does before fixing' -- is now ANSWERED, so this is no longer a research task:

    SELECT LENGTH(SPACE(18446744073709551615))        mysql NULL    engine 0
    SELECT LEFT('abc', 18446744073709551615)          mysql 'abc'   engine ''
    SELECT RIGHT('abc', 18446744073709551615)         mysql 'abc'   engine ''
    SELECT LENGTH(LPAD('a', 18446744073709551615,'x')) mysql NULL   engine 0
    SELECT LENGTH(RPAD('a', 18446744073709551615,'x')) mysql NULL   engine 0

So MySQL splits two ways, and the engine is wrong in both: SPACE/LPAD/RPAD answer NULL (they try to build the string and give up), while LEFT/RIGHT clamp to the whole string. The guess in the description that 'answering 0 is at least cheap' is not what MySQL does in either group.
