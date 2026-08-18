---
id: 23
title: IN list of scalar subqueries
state: Done
parent: 4
labels: [compat, parser]
created: 2026-08-18T18:52:11Z
updated: 2026-08-18T19:41:19Z
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

## Notes

### 2026-08-18T19:41:18Z — Christopher Vachon (user)

Closed 2026-08-18. The list is rewritten before parsing into IN ((SELECT a) UNION ALL (SELECT b)), which is equivalent and already supported end to end. The branches keep their parentheses: several in the corpus end in LIMIT 1, and SELECT ... LIMIT 1 UNION ALL SELECT ... binds the LIMIT to the whole union and fails to parse. Only a list of two or more parenthesised SELECTs is rewritten, so ordinary value lists and IN (SELECT ...) are untouched.

With this the engine's front end parses every one of the 3,161 statements in the 604 migrations and every one of the 1,191 in the provisioning dumps. The three remaining rejections across the corpus are all artifacts of recovering SQL from Prisma.sql templates, where an interpolation stands for a whole clause, so the complete statement never exists in source.
