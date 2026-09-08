---
id: 257
title: A foreign key constraint name is not required to be unique in its database
state: Done
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-08T14:30:10Z
updated: 2026-09-08T17:55:25Z
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

## Notes

### 2026-09-08T17:55:25Z — Christopher Vachon (user)

FIXED in 60054fd.

A foreign key's name and a CHECK's are scoped to the DATABASE, not the table. 1826 and 3822, both measured on 8.3, and the issue's own premise needed correcting on two points:

  * The issue said both the across-tables and the same-table case are 1826. The same-table case is 1061, because adding a key makes an index of its own name and the index answers first. The engine already agreed there. Measured the discriminator: with the index name freed by a rename, the same-table case IS 1826, so the constraint rule is real rather than the index rule wearing another number.
  * The issue guessed 3822 for CHECK "presumably". Confirmed -- and the two namespaces are SEPARATE: a check may take a name a key holds and the other way round, both measured.

Also measured: the comparison ignores case and reports the incoming spelling; another database is fine; a CREATE TABLE declaring the name inline is refused too; and 1826 fires even when the key needs no index of its own.

WHAT THIS TURNED UP IN #0255, which is the part worth remembering. Measuring for the above showed three things that change had got wrong, every one of them from a review finding accepted without asking 8.3:
  * supersession is COLUMN COVERAGE ALONE and consults no foreign key -- 8.3 drops a key's index after the key is dropped, and on a LIKE copy with no keys at all;
  * CREATE TABLE ... LIKE CARRIES the mark rather than clearing it;
  * DROP FOREIGN KEY does not clear it (unmark_provisional deleted);
  * RENAME INDEX does, and is the only thing besides a takeover that ends it.

REVIEW OF THIS CHANGE FOUND SIX MORE, all fixed:
  * both rename spellings bypassed the new rule, because a rename rewrites the keys named after the table. rename_tables now simulates constraint names pair by pair before writing anything; alter_table renames on its copy before judging.
  * the dry run read the TYPED spelling where the write reads the stored one, so `RENAME TABLE a TO b` over a stored `A` saw no collision and made one.
  * an unnamed inline CHECK was named `<column>_chk` instead of `<table>_chk_<n>` -- a wrong name that this rule turned into a wrong refusal, so two tables with the same inline check on the same column stopped being creatable.
  * CREATE TABLE ... LIKE refused any source with a check. 8.3 regenerates every check name for the copy, including explicitly written ones.
  * a generated check name now follows its table through a rename.

AND THE RE-REVIEW FOUND A REGRESSION I INTRODUCED BY THAT LAST FIX: teaching renames to rewrite check names without teaching the dry run about them let a rename collide two checks. That wedged BOTH tables -- every later ALTER TABLE on either was refused, and only dropping one cleared it. Checks are simulated in a pool of their own now, because folding them in with the keys would refuse a check that merely shares a key's name, which 8.3 accepts.

Filed rather than fixed: #0259, #0260, #0261, #0262.

Differential 5662 statements, 0 divergences. All 604 migrations of the real history still apply.
