---
id: 211
title: A non-JSON value assigned to a JSON column is accepted
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T22:31:37Z
updated: 2026-09-04T01:56:12Z
---

## Description

Found while implementing #0084/#0121.

    INSERT INTO T (doc) VALUES (2.5)   -- doc is JSON
    8.3     3140 Invalid JSON text: ... not a JSON text, may need CAST
    engine  accepted, stores a number

Measured for a DECIMAL, an INT and a DOUBLE. So the engine holds a JSON column whose contents MySQL would have refused -- the same class of wrong stored data #0194/#0195 fixed for width and range, on the one column type they did not cover.

Priority 2: the target application declares 54 JSON columns and writes them through Prisma, which sends a JSON string; but a raw query or a migration that assigns a number would diverge silently.

The 3140 wording also differs where both refuse: 8.3 says '"expected value at line 1 column 1" in column ''doc''' against the engine's '"Invalid value." at position 0 in value for column ''T.doc''.'

## Notes

### 2026-09-04T01:56:11Z — Christopher Vachon (user)

Landed in e7ee480. TWO codes, not one: 3140 for a value that is not JSON text, and 3144 'Cannot create a JSON value from a string with CHARACTER SET binary' for a binary string -- which 8.3 refuses EVEN WHEN THE BYTES SPELL A VALID DOCUMENT.

This is the one column rule outside strictness: neither sql_mode='' nor INSERT IGNORE downgrades it, unlike every width and range rule from #0194/#0195.

Message shape still differs where both refuse (8.3 carries a position and qualifies the column with its table); left alone because store() is given only a column name.
