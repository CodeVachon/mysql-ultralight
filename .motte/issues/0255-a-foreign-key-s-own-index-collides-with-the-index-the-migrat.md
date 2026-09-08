---
id: 255
title: A foreign key's own index collides with the index the migration then creates
state: Todo
parent: 9
labels: [executor, app-priority-1]
created: 2026-09-08T14:19:50Z
updated: 2026-09-08T14:19:50Z
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
