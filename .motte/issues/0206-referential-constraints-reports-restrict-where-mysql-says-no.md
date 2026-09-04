---
id: 206
title: referential_constraints reports RESTRICT where MySQL says NO ACTION
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T18:51:31Z
updated: 2026-09-04T04:48:45Z
---

## Description

Found while implementing #0179/#0180/#0184. The corpus group added there excludes these two columns with a comment saying why -- 18 divergences when first included.

    an FK with no ON DELETE / ON UPDATE written
    information_schema.referential_constraints.DELETE_RULE / UPDATE_RULE
    mysql   NO ACTION
    engine  RESTRICT

Also a metadata difference on the same columns: they are an ENUM on 8.3 and a plain string here (flags mysql NOT_NULL|BINARY|ENUM, engine NOT_NULL|BINARY).

Priority 2 because the target application reads information_schema.referential_constraints 26 times -- more than any other information_schema table -- and declares 824 foreign keys, 785 of them with ON UPDATE CASCADE and 300 with ON DELETE SET NULL. The unwritten ones are exactly the case that diverges.

## Notes

### 2026-09-04T04:48:45Z — Christopher Vachon (user)

Landed in afbfcbd. The unwritten action was the only spelling that diverged, and the two rules are INDEPENDENT -- ON DELETE CASCADE alone is NO ACTION / CASCADE on 8.3 and was RESTRICT / CASCADE here.

Two more columns of the same row were wrong and nobody had asked about them: UNIQUE_CONSTRAINT_SCHEMA named the child's database rather than the parent's, and UNIQUE_CONSTRAINT_NAME was hard-coded PRIMARY where 8.3 names the key that actually covers the parent -- NULL when only a plain index does, PRIMARY first when both could.

All three rule columns are ENUMs on the wire, not strings, and the ENUM flag is visible to a client.

A review finding was measured and REJECTED: it claimed InnoDB refuses SET DEFAULT. 8.3 accepts it, and a corpus case now proves it.

Both corpus exclusions this issue's notes recorded are removed. Running the corpus under a NO_ACTION mutation gives 52 divergences, which is what says the removal bites.
