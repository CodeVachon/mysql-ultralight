---
id: 261
title: A generated CHECK name is numbered past a written one
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-08T17:36:18Z
updated: 2026-09-08T17:36:18Z
---

## Description

Measured on 2026-09-08 while fixing #0257, with scripts/ask.sh. Not introduced by that change and not fixed by it.

    CREATE TABLE T (a INT, CONSTRAINT T_chk_1 CHECK (a > 0), CHECK (a < 9));
    mysql   3822 Duplicate check constraint name 'T_chk_1'
    engine  accepted, the second check named T_chk_2

8.3 numbers a generated check name by the constraint's POSITION among the table's checks, so the second one is `T_chk_2`... no: it generates `T_chk_1` for the second check and finds the name taken, which is why it is 3822. This engine's `auto_check_name` takes one past the HIGHEST existing `_chk_<n>` suffix, so it skips to `T_chk_2` and accepts.

The engine's rule is deliberate and documented on `auto_check_name`: counting rather than taking the highest was measured to reuse a name after a drop, and the doc records `C_chk_1`/`C_chk_2` losing `C_chk_1` and the next check still being `C_chk_3` on 8.3. So 8.3 is NOT simply positional either, and the two behaviours have to be reconciled by measurement rather than by picking one:

  * what does 8.3 name the next check after `C_chk_1` and `C_chk_2` lose `C_chk_1`?  (recorded: `C_chk_3`)
  * what does it do when the *written* name occupies the number the generator is about to use, as above?  (measured: 3822)

A rule that fits both is probably "generate `<table>_chk_<highest+1>`, then refuse if that name is taken" -- but the engine already generates highest+1, so that alone does not explain the 3822. Measure before implementing.

Priority 3: the divergence is an acceptance, not a wrong answer, and it needs a table that both writes an explicit `<table>_chk_<n>` name and leaves another check unnamed. Prisma writes neither.
