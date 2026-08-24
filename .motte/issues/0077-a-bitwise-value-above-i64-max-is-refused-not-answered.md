---
id: 77
title: A bitwise value above i64::MAX is refused, not answered
state: Todo
parent: 9
labels: [executor]
created: 2026-08-24T17:04:58Z
updated: 2026-08-24T17:04:58Z
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
