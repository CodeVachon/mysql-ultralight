---
id: 255
title: A foreign key's own index collides with the index the migration then creates
state: Done
parent: 9
labels: [executor, app-priority-1]
created: 2026-09-08T14:19:50Z
updated: 2026-09-08T15:18:36Z
---

## Description

BLOCKS THE TARGET APPLICATION. `prisma migrate deploy` against the real 604-migration history fails at migration 23, `20231103151653_rate_type_property_value`:

    Database error code: 1061
    Duplicate key name 'Job_rateTypeId_fkey'

The migration does this, which is a shape Prisma emits routinely:

    ALTER TABLE `Job` ADD CONSTRAINT `Job_rateTypeId_fkey`
      FOREIGN KEY (`rateTypeId`) REFERENCES `PropertyValue` (`id`)
      ON DELETE SET NULL ON UPDATE CASCADE;
    CREATE INDEX `Job_rateTypeId_fkey` ON `Job` (`rateTypeId`);

The engine refuses the CREATE INDEX because the foreign key already made an index of that name. 8.3 accepts it.

MEASURED ON 8.3, 2026-09-08, with scripts/ask.sh. The rule is not "duplicates are allowed"; a foreign key's backing index is PROVISIONAL and any explicit index that can serve the foreign key replaces it:

  A  FK index c(r), then CREATE INDEX c ON J(r)      ACCEPTED. One index remains: c on r.
  B  FK index c(r), then CREATE INDEX c ON J(s)      1061. Same name, different column, real collision.
  C  FK index c(r), then CREATE INDEX other ON J(r)  ACCEPTED, and the implicit c is DROPPED --
                                                     only `other` remains, and it serves the FK.
  D  plain INDEX c(r), then CREATE INDEX c ON J(r)   1061. No FK involved, ordinary duplicate.

So A and C both have to change, not just A. C matters on its own: get it wrong and the engine keeps two indexes where MySQL has one, which is visible in information_schema.STATISTICS -- and Prisma introspects exactly that.

THE DOC COMMENT ON `add_index` IN crates/engine/src/ddl.rs IS WRONG AND SAYS IT WAS MEASURED: it claims 1061 is 8.3's answer "including the index a foreign key creates for itself". Case A above is that exact sentence, measured, and 8.3 accepts it. Whatever was measured, it was not this.

Likely introduced by "Refuse an index name the table already carries", since README.md's claim that all 604 migrations apply was true when written.

IMPLEMENTATION NOTE: telling A from D needs the catalog to know which indexes a foreign key created for itself, which today it does not. That is a new field on Index, and the state image format is append-only -- a new field means a new FORMAT_VERSION (13 today).

Priority 1: this is the whole point of the project. The target application cannot get past its own migration history.

## Notes

### 2026-09-08T15:18:36Z — Christopher Vachon (user)

FIXED in 2bf51eb. `prisma migrate deploy` against the real history now applies 406 of 604 migrations, up from 22. The next blocker is #0256.

The rule, all of it measured on 8.3 rather than reasoned about: the index a foreign key makes for itself is provisional, and anything that can serve the key takes it over.

  after a key `c` on `(r)`      CREATE INDEX c ON J(r)       takes over, one index left
                                CREATE INDEX c ON J(s)       1061, cannot serve the key
                                CREATE INDEX other ON J(r)   takes over, `c` is gone
                                the same CREATE INDEX twice  the second is 1061
  an explicit `c` a key reused  CREATE INDEX c ON J(r)       1061, never provisional
  ADD PRIMARY KEY (r)                                        takes over
  a second key on `(r, b)`                                   takes over

The last two were implemented as exclusions on the assumption that they would NOT take over, and 8.3 said otherwise on both. The two before them are why this is a remembered field rather than a derived one -- after a takeover the index is ordinary, and an explicit index is ordinary from the start, so neither is distinguishable from the provisional case by name and columns.

`Index::implicit_for_foreign_key`, FORMAT_VERSION 13 -> 14. A v13 image would reload every index ordinary and re-arm this for anyone booting with `--state`, so it has its own paragraph in the format history and a round-trip test that fails when the field is not written.

REVIEW FOUND SEVEN THINGS, ALL FIXED. Worth keeping the first:
  * The takeover ran BEFORE the refusals, and `create_index` holds the live catalog rather than a clone the way `alter_table` does. A refused `CREATE UNIQUE INDEX` therefore destroyed the index its foreign key was relying on -- 1062 returned, and the key left with no index at all, which is the state 1553 exists to prevent. Now the set is computed first and removed only once nothing can still refuse.
  * `CREATE INDEX IF NOT EXISTS` reported success, created nothing, and left the index provisional for a later statement to delete.
  * The `!primary` filter was dropped in passing, which would have made `CREATE INDEX PRIMARY` 1061 where 8.3 answers 1280 -- reversing a decision the code documents.
  * `CREATE TABLE ... LIKE` carried the mark onto a copy that has no constraints; `DROP FOREIGN KEY` and `DROP CONSTRAINT` left it on an index that had outlived its key.
  * Three of those fixes had no test that bit. mutate.sh confirms all three do now.

ONE FINDING WAS REFUTED BY MEASUREMENT rather than fixed: a renamed provisional index is not taken over by a new index under the old name. 8.3 keeps both, and so does this engine -- the finding's "should leave one" was an assumption. Chasing it found a real bug anyway, now fixed here: RENAME INDEX onto a name the table already carries is 1061 on 8.3 and was accepted, which left two indexes of one name -- and a takeover removing by name would have removed both, leaving a key with nothing. A rename to the name it already has is accepted, measured after writing down the opposite.

Differential: 5570 statements, 0 divergences, with a group covering every row of the table above.
