---
id: 208
title: A foreign key over a primary-key column gets a redundant index
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T18:51:31Z
updated: 2026-09-03T18:51:31Z
---

## Description

Found while implementing #0179/#0180/#0184.

    CREATE TABLE Ic (pid INT NOT NULL PRIMARY KEY,
                     CONSTRAINT fki FOREIGN KEY (pid) REFERENCES P(id))
    mysql   PRIMARY alone
    engine  fki + PRIMARY

Root cause: create_table adds the inline primary-key index AFTER apply_constraint runs, so the covered-by-an-existing-index test cannot see it.

Consequence beyond the extra row in information_schema.statistics: DROP PRIMARY KEY on such a table is ACCEPTED here, because the redundant fki still backs the key, where 8.3 answers 1553. So the #0180 rule is correct but reaches the wrong conclusion on this shape.

Also found: the standalone CREATE INDEX ... ON t (nosuch) still creates a phantom index over a missing column -- the 1072 check added for #0179 is on the ALTER TABLE path only.
