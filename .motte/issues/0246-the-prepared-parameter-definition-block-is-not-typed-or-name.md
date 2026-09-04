---
id: 246
title: The prepared parameter definition block is not typed or named
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T08:53:05Z
updated: 2026-09-04T08:53:05Z
---

## Description

Found while implementing #0228, which fixed the COLUMN definition block that COM_STMT_PREPARE returns. The PARAMETER definition block is the other half and is untouched.

    engine  ?0 / ?1, VAR_STRING len 4096 dec 0 cs 255 for every parameter
    mysql   named for the marker or the alias, and typed by the rule #0228 established

Lives in crates/server and crates/protocol rather than the engine's describe path, which is why #0228 was scoped to the columns.

A client that inspects parameter metadata before binding -- to choose how to encode a value -- sees the wrong type. Priority 3 because Prisma binds by position without reading it.
