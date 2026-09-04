---
id: 251
title: A table cannot be moved to another database
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T21:31:56Z
updated: 2026-09-04T21:31:56Z
---

## Description

The half of #0232 that was refused rather than implemented.

    ALTER TABLE B2 RENAME TO other.Alt
    RENAME TABLE B2 TO other.Alt
    mysql   moves the table to database 'other', rows and all
    engine  1235 does not yet support moving `B2` to database `other`

Before #0232 the ALTER form did something worse than refuse: it built a table literally named 'other.Alt' in the CURRENT database, so the rows existed and no qualified name could reach them. That is fixed -- the refusal is clear and the table stays where it was, addressable.

Implementing the move means carrying the rows, the indexes, the table's own foreign keys AND every foreign key pointing at it from a third database, atomically across two databases. #0205 made the parent's schema and name separate fields on ForeignKey (FORMAT_VERSION 12), so the machinery to describe a cross-database reference exists; what is missing is moving one.

Priority 3: the target application uses a single database, so nothing it runs reaches this.
