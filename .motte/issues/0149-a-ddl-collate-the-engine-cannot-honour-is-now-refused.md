---
id: 149
title: A DDL COLLATE the engine cannot honour is now refused
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-01T06:09:09Z
updated: 2026-09-03T14:20:30Z
---

## Description

Recorded during #0137/#0138, which introduced the refusal deliberately.

Before that change a COLLATE clause in DDL was accepted and then ignored, which produced two wrong answers in opposite directions over a VARCHAR(10) COLLATE utf8mb4_bin under a UNIQUE key:

    INSERT 'a' then 'A'    mysql stores both   engine 1062
    WHERE a = 'A'          mysql returns A     engine returned a

Both are gone because the clause is refused, matching what the expression form already did, and both DDL and expression now share one reader -- two readers of one clause is how they came to disagree.

So this is a narrowing, not a fix: 8.3 accepts the statement and the engine now refuses it. That is the house rule (a clear error beats a wrong answer) but it turns working DDL into an error, which is the direction that breaks a schema outright. Deliberately not in the differential corpus, since it diverges by construction.

It costs nothing against the conformance target today -- all 314 COLLATE clauses in the Appello schema are _ci, which the engine honours -- but a schema using a _bin collation cannot be created at all.

Closing this means honouring a per-column collation, which is #0129. When that lands, the refusal should become an answer and a corpus case should replace it.

## Notes

### 2026-09-01T07:46:39Z — Christopher Vachon (user)

Widened by 294e19c: the refusal now covers the TABLE-level clause as well as the column-level one, through a single shared reader. Review found the first cut refused only the column form while the table form was still parsed and dropped -- and this repo's example migrations write it that way on all five tables, so the wrong answer was reachable by writing the clause one line higher.

The boundary is now measured rather than suffix-matched: 286 collation names read off 8.3, embedded sorted. A name outside that set is 1273 as MySQL writes it. A real MySQL collation this engine cannot compare in -- utf8mb4_bin, latin1_swedish_ci, binary -- is the unsupported refusal, which is what this issue tracks. The first cut conflated the two and answered 1273 for utf8mb4_bin, a false claim about a name MySQL has.

So closing this issue means honouring those collations (#0129), and when it lands the refusal becomes an answer at both levels and the corpus can carry cases for them.

### 2026-09-03T14:05:52Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3. Premise holds, and the description's table shows the OLD engine behaviour -- worth updating before anyone works it.

Today the engine refuses the DDL outright:
    CREATE TABLE Cb (a VARCHAR(10) COLLATE utf8mb4_bin, UNIQUE KEY (a))
    mysql   accepted; then 'a' and 'A' both store, and WHERE a='A' returns A
    engine  1235 'does not yet support the utf8mb4_bin collation' -- so the table never exists

The description's '1062 on the second INSERT' and 'WHERE a = A returned a' describe the behaviour before the refusal was added, which is what the title means by 'is now refused'. The current divergence is the refusal itself, not a wrong answer.
