---
id: 257
title: A foreign key constraint name is not required to be unique in its database
state: Todo
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-08T14:30:10Z
updated: 2026-09-08T14:30:10Z
---

## Description

Found by the differential harness on 2026-09-08 while adding coverage for #0255, and confirmed in isolation with scripts/ask.sh. The harness found it because every case runs in both protocols against one database, so a group's DDL runs twice -- which is the second run 8.3 refuses and this engine does not.

    CREATE TABLE Dp (id INT NOT NULL PRIMARY KEY);
    CREATE TABLE D1 (id INT NOT NULL PRIMARY KEY, r INT NULL);
    CREATE TABLE D2 (id INT NOT NULL PRIMARY KEY, r INT NULL);
    ALTER TABLE D1 ADD CONSTRAINT dup FOREIGN KEY (r) REFERENCES Dp (id);
    ALTER TABLE D2 ADD CONSTRAINT dup FOREIGN KEY (r) REFERENCES Dp (id);
    ALTER TABLE D1 ADD CONSTRAINT dup FOREIGN KEY (id) REFERENCES Dp (id);

    mysql   1826 Duplicate foreign key constraint name 'dup', on BOTH the
            second and the third statement
    engine  all three accepted

A foreign key's name is scoped to the SCHEMA, not to the table -- unlike an index name, which is scoped to its table. Both the across-tables case and the same-table case are refused by 8.3.

crates/engine/src/error.rs already says so, in the doc comment on `duplicate_key_name`: "unlike a foreign key's or a check's, which are scoped to the schema and refused with 1826 and 3822 instead". Neither code is ever raised. 3822 for CHECK is presumably the same gap and is worth measuring alongside.

CONSEQUENCE BEYOND THE ERROR CODE: the accepted duplicates are then reported by information_schema.KEY_COLUMN_USAGE as duplicate rows -- measured, D1 appears twice for one name. Prisma's migration engine reads that back and compares it against the schema, so a table can read as drifted.

Priority 2 rather than 1: Prisma names its constraints after the table and column, so its own migrations do not collide, and the 604-migration history does not reach this. Anything hand-written might.
