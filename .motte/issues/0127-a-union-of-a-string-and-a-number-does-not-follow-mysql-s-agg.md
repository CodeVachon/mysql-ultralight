---
id: 127
title: A union of a string and a number does not follow MySQL's aggregate string rule
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-08-26T08:49:10Z
updated: 2026-09-03T14:20:28Z
---

## Description

Found while measuring for #0073, pre-existing and not caused by it.

    SELECT * FROM (SELECT NULL UNION SELECT NULL) d UNION SELECT 1
    mysql   MYSQL_TYPE_STRING, char(8), cs 45
    engine  MYSQL_TYPE_VAR_STRING

crates/engine's widen falls to VAR_STRING wherever a union mixes a string with something else, where MySQL keeps CHAR and computes a length from the widest branch -- here 8, the display width of a BIGINT.

Related to #0065, which taught a union to keep a column's type when a branch is NULL, and to #0123, which is the same aggregation applied to a decimal's scale. This is the string half of the same rule.

Not in the corpus. Note the length half cannot be proven by the harness until it compares column_length at all -- see the length issue filed alongside this one.

## Notes

### 2026-08-28T17:00:29Z — Christopher Vachon (user)

Not provable by #0126's widening, stated plainly there: the harness compares column_length only for a column MySQL reports with a non-empty org_table, and a union column has an empty one, so its width is out of scope.

That matters for this issue because the length is half its content -- MySQL declares char(8) where the engine says VAR_STRING, and the 8 is a computed width. Closing it needs either the origin-field comparison turned on (2924 divergences over 583 statements as measured in #0126) or a separate route to compare a computed column's width, which is #0111's Item::max_length gap.

### 2026-09-03T14:03:11Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. SELECT * FROM (SELECT NULL UNION SELECT NULL) d UNION SELECT 1 is MYSQL_TYPE_STRING Length 8 on 8.3 and VAR_STRING Length 4096 here. Note the description's 'cs 45' depends on the client's connection charset -- with the connection pinned to utf8mb4 the collations agree, so the durable claims are the type code and the length.
