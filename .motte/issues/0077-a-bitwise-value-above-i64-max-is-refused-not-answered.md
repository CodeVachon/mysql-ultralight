---
id: 77
title: A bitwise value above i64::MAX is refused, not answered
state: Done
parent: 9
labels: [executor]
created: 2026-08-24T17:04:58Z
updated: 2026-08-25T08:17:18Z
---

## Description

Split out of #0052, which implemented & | ^ << >> and the unary ~. The evaluation composes over u64 correctly, but Value is an i64, so a result that does not fit is raised as a 1235 rather than answered:

    SELECT ~0        mysql 18446744073709551615   engine ERROR 1235
    SELECT 1 << 63   mysql 9223372036854775808    engine ERROR 1235
    SELECT -5 & -3   mysql 18446744073709551611   engine ERROR 1235
    SELECT ~n        for any n >= 0

A clear error rather than a wrong answer, which is the rule -- before the refusal went in, these answered -1 and the engine's own two protocols disagreed about what -1 meant. Everything landing in 0..=i64::MAX still answers, including chains through wider intermediates: ~0 & 255 is 255 and (1<<63)>>63 is 1, because only the value the chain hands out is checked, not each operator.

Fixing it means an unsigned variant on Value. That is not a local change: crates/engine/src/image.rs is append-only, so a new variant means a new FORMAT_VERSION; every comparison, every function match arm and both wire encodings have to learn it. The same variant would close the mixed-context gaps -- ~0 MOD 10, ~0 > 5, CONCAT(~0), ~0 UNION SELECT 1 -- which are refused today for the same reason.

Nothing in the conformance corpus or the target schema uses a bitwise result above the signed range.

## Notes

### 2026-08-25T08:17:18Z — Christopher Vachon (user)

Closed by #0069 in 3b6a342. Value::Unsigned(u64) is the variant this issue said was needed, added there because the unsigned range check could not be built without it. SELECT ~0, 1 << 63, -5 & -3 and ~n all answer, as do every mixed reading listed here: ~0 MOD 10, ~0 > 5, CONCAT(~0) and ~0 UNION SELECT 1. The tests written here as refusals were rewritten as answers.

One reader is still approximate and is documented where the tag is: ~0 / 2 gives 9223372036854776000 against MySQL's 9223372036854775807.5000, because division goes through an f64 -- as 9223372036854775807 / 2 always did, long before the tag. That is #0085, not this.
