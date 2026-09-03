---
id: 151
title: Identifier comparison is case-insensitive where the reference server is not
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-01T23:47:05Z
updated: 2026-09-03T14:20:30Z
---

## Description

Chosen deliberately during #0098 and recorded so the choice is visible.

The mysql:8.3 container runs lower_case_table_names=0, so database and table names are case-sensitive there:

    SELECT j2.b FROM J2                                             mysql 1054   engine answers
    SELECT PROBE.J2.b FROM J2                                       mysql 1054   engine answers
    SELECT INFORMATION_SCHEMA.COLUMNS.TABLE_NAME FROM information_schema.columns   mysql 1054   engine answers

#0098 kept the engine case-insensitive, which is consistent with every other identifier comparison it makes -- i.e. it behaves as lower_case_table_names=1 throughout -- and wrote the reason into ColumnRef::matches. No corpus case names a qualifier in the wrong case.

Distinct from #0148, which is about the collation of data in information_schema, not about identifier matching.

Worth deciding rather than drifting: MySQL's own behaviour depends on the server variable and on the host filesystem, so 'match MySQL' has no single answer here. The options are to model lower_case_table_names, or to state the engine's choice in the README and keep it uniform. Note a case-sensitive engine would refuse schemas that work on macOS and Windows deployments, which is the direction that breaks things.

## Notes

### 2026-09-03T14:04:20Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed, and the container premise checks out: @@lower_case_table_names is 0 on the mysql:8.3 image, so SELECT j2.b FROM J2 is 1054 there and answers 1 here. Separately noticed: the engine returns @@lower_case_table_names as a string where 8.3 returns a number -- the same system-variable typing problem recorded on #0095.
