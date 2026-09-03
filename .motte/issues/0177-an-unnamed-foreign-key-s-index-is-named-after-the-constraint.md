---
id: 177
title: An unnamed foreign key's index is named after the constraint, not its first column
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T10:37:35Z
updated: 2026-09-03T10:37:35Z
---

## Description

Found by review while implementing #0150, and left out of it because a test in the tree asserts the current behaviour.

MySQL gives an unnamed foreign key a constraint called `<table>_ibfk_N` and an index named after its first column. Measured on mysql:8.3 for `CREATE TABLE C2 (id INT NOT NULL, pid INT NOT NULL, qid INT NOT NULL, PRIMARY KEY (id), FOREIGN KEY (pid) REFERENCES P2(id), FOREIGN KEY (qid) REFERENCES P2(id))`:

- `information_schema.table_constraints`: `PRIMARY`, `C2_ibfk_1`, `C2_ibfk_2`
- `SHOW INDEX`: `PRIMARY`, `pid`, `qid`

This engine names the index `C2_ibfk_1` and `C2_ibfk_2` — `crates/engine/tests/sql.rs::unnamed_foreign_keys_get_distinct_names` asserts exactly that, so fixing this means rewriting that test around the constraint names rather than the index names.

Since #0150 the name decides what collides, so it cuts both ways: `INDEX F1_ibfk_1 (b), FOREIGN KEY (a) REFERENCES P(id)` is 1061 here and accepted on 8.3, and `INDEX a (b), FOREIGN KEY (a) REFERENCES P(id)` stores `F2_ibfk_1` where 8.3 stores `a_2`. The right name is `auto_index_name(table, &key_columns)`, which `ddl.rs` already has; the constraint keeps `<table>_ibfk_N`, which is right and which `foreign_keys` should go on holding.

An *explicitly* named foreign key already agrees with 8.3 — its index takes the constraint's name there too — and that is the shape the differential corpus covers.
