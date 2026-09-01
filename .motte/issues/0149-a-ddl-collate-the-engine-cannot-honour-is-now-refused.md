---
id: 149
title: A DDL COLLATE the engine cannot honour is now refused
state: Todo
parent: 9
labels: [compat]
created: 2026-09-01T06:09:09Z
updated: 2026-09-01T06:09:09Z
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
