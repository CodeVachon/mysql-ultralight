---
id: 23
title: IN list of scalar subqueries
state: Todo
parent: 4
labels: [compat, parser]
created: 2026-08-18T18:52:11Z
updated: 2026-08-18T18:52:11Z
---

## Description

The last statement in the 4,661 the conformance corpus contains that the engine's front end cannot parse.

  AND documentAttributeId IN (
      (SELECT id FROM DocumentAttribute WHERE name = 'Allergies' LIMIT 1),
      (SELECT id FROM DocumentAttribute WHERE name = 'Emergency Contact Phone' LIMIT 1),
      ...
  )

MySQL accepts an IN list whose elements are scalar subqueries. sqlparser-rs reads the opening parenthesis followed by SELECT as an IN (subquery) and then fails on the comma: 'Expected: ), found: ,'.

Found in apps/api/prisma/migrations/20251104210900_user_datamodel_003_form_elements/migration.sql. One statement out of 4,661, so it is recorded rather than worked around; closing it needs either a parser fork or a rewrite that splits the list into an OR chain of equality comparisons.
