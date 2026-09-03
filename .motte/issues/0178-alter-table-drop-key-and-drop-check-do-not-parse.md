---
id: 178
title: ALTER TABLE DROP KEY and DROP CHECK do not parse
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T12:29:35Z
updated: 2026-09-03T12:29:35Z
---

## Description

Found while implementing #0133, measured on 8.3.

    ALTER TABLE t DROP KEY nosuch     mysql 1091 Can't DROP 'nosuch'; check that column/key exists
    ALTER TABLE t DROP CHECK nosuch   mysql 3821 HY000 Check constraint 'nosuch' is not found in the table.
    engine                            1064 parse error for both

sqlparser 0.62 has no KEY spelling of the drop operation and no DROP CHECK; both fall through to DROP [COLUMN] and choke. DROP CHECK of an *existing* check is equally unreachable, so this is not only a wrong error -- there is no way to remove a check by that spelling at all.

Needs sqlparser work rather than engine work, which is why #0133 stopped at the boundary.
