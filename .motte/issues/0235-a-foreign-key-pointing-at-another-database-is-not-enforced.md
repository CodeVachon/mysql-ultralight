---
id: 235
title: A foreign key pointing at another database is not enforced
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T04:38:07Z
updated: 2026-09-04T04:38:07Z
---

## Description

Found while implementing #0205/#0206, pre-existing.

The parent was resolved inside the CHILD's own database, so a cross-database foreign key bound to nothing and was never enforced -- and a same-database qualified key bound only by luck.

#0205 stored the parent's schema and name as separate fields (FORMAT_VERSION 12) and made resolution compare BOTH halves, so the key can no longer bind to the wrong parent. But a genuinely cross-database key still resolves to nothing rather than to the parent in the other database, so it remains unenforced -- now explicitly rather than accidentally.

The target application uses a single database, which is why this is priority 3.
