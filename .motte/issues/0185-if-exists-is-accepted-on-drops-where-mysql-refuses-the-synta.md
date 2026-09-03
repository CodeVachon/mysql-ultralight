---
id: 185
title: IF EXISTS is accepted on drops where MySQL refuses the syntax
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T12:30:07Z
updated: 2026-09-03T12:30:07Z
---

## Description

Found while implementing #0133.

DROP COLUMN IF EXISTS, DROP INDEX IF EXISTS, DROP FOREIGN KEY IF EXISTS, DROP CONSTRAINT IF EXISTS and the standalone DROP INDEX IF EXISTS x ON t are all 1064 on 8.3 -- MySQL has no IF EXISTS on these. sqlparser parses them and the engine honours them.

So a migration using IF EXISTS to make a drop tolerant passes here and is a syntax error against a real server, and the tolerance it was written for silently applies in tests and never in production. Accepting syntax MySQL rejects is the mirror of the case #0133 fixed.
