---
id: 113
title: div_precision_increment is not a system variable
state: Done
parent: 9
labels: [compat]
created: 2026-08-25T23:13:03Z
updated: 2026-09-02T19:46:45Z
---

## Description

`SET div_precision_increment = 6` is 1193 `Unknown system variable` here, and `SELECT @@div_precision_increment` is the same. MySQL has it as a session variable with a documented default of 4, and it decides two things this engine hard-codes:

- a quotient's scale, which is the left operand's plus the increment (#0085, fixed with the constant)
- an average's scale, which is the scale it averaged plus the increment (#0049, likewise)

Measured on 8.3: `SET div_precision_increment = 6` makes `1/3` 0.333333 and `1.5/1.5` 1.0000000; at 0 it makes `1/3` 0 and `7/2` 3 — the second is 3 rather than 4 because MySQL computes the quotient to a whole number of nine-digit words and truncates there, and at an increment of 0 there are no words at all. `DIV` and `MOD` are unaffected at any setting.

`crates/engine/src/decimal.rs` has it as `DIV_PRECISION_INCREMENT`, one constant that `AVERAGE_EXTRA_SCALE` is defined from, so there is one place to read the session from once there is a session to read. `crates/engine/src/expr.rs::arithmetic` takes no `Context`, which is the plumbing this needs.

Refusing the SET is the honest half of the gap and is why this is not urgent: nothing silently gets the wrong answer, a client that asks is told the variable does not exist.

## Notes

### 2026-09-02T19:46:45Z — Christopher Vachon (user)

Done in df06879.

Bounds measured rather than assumed: 0..=30, clamped with warning 1292 rather than refused (31 -> 30, -1 -> 0, u64::MAX -> 30), and 1232 for a non-integer -- including a string spelling a number, so '3', 3.7 and NULL error while TRUE gives 1 and 1 + 1 gives 2. The same rule default_week_format follows, so they share a clamped_integer helper; only the upper bound differs. Settable at SESSION and GLOBAL; had to be added to SHOW VARIABLES for #0057's reason, or it materialises only on first write.

Sites the constant was read from: the quotient's interim word count and result scale in decimal.rs, and AVG via AVERAGE_EXTRA_SCALE in exec.rs. Nothing else derived from it.

New measurement: AVG goes through the same division, word-boundary truncation included, which the old divide_units path did not -- an average of 1.00001 across three rows is 0.333336666 on 8.3 where rounding gives ...667. Latent, never asked by the corpus, fixed here because it is the same number.

The merge is the part worth recording. The agent could not run differential.sh (another agent held the container), so it verified by hand-diffing transcripts against a real 8.3 and said plainly that the binary protocol and column metadata were still unverified. On merge the harness reported 2 divergences in the binary protocol -- and the cause was the corpus, not the engine.

A client caches a prepared statement by its text for the life of a connection, and MySQL fixes a quotient's scale when the statement is PREPARED; a later SET does not re-fix it. Three groups shared two statement texts, so the later groups' binary passes asked MySQL about the first group's increment. MySQL was the one stuck at 0. It bit only the literal-only quotient: the AVG statements name a table that reset drops and recreates, and MySQL re-prepares a statement whose table was replaced.

Every statement in these groups now carries its own alias, and the rule is in the README under 'Writing a case' -- the harness cannot enforce it, and the run it produces looks exactly like an engine divergence.

The engine is deliberately NOT made to freeze the increment at prepare time. Modelling MySQL's prepare-time item fixing for one variable while sql_mode and the rest keep re-deriving would make the gap depend on which variable was changed, which is the argument #0085's doc comment already makes.

Left to #0095, not fixed here: an overridden variable reads back as VAR_STRING where MySQL sends LONGLONG UNSIGNED. That is about any variable, not this one, which is why no SELECT @@div_precision_increment case exists -- reset sets it on every connection, so it is always overridden.

8 mutations, all bite. The last one needed two groups sharing a text: mutating one alias alone left the run green, because a single group re-prepares under its own setting. mutate.sh's under-mutation guard caught it.

Corpus 2454 -> 2568, baseline empty, both gates verified independently on the merged tree.
