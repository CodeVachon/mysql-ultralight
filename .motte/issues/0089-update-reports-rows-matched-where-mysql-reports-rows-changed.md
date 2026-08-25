---
id: 89
title: UPDATE reports rows matched where MySQL reports rows changed
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T03:02:14Z
updated: 2026-08-25T03:02:14Z
---

## Description

Found by review during #0068, via a corpus case that was then removed rather than blessed.

UPDATE t SET x = x over rows that already hold that value reports every row as affected; MySQL reports 0, because it counts rows actually changed rather than rows matched. The distinction is visible to any client that reads affected_rows -- Prisma among them, which uses the count to decide whether an update found its target.

The engine writes the value unconditionally and counts the write. Making it right means comparing the new value against the old before counting, which is also what CLIENT_FOUND_ROWS exists to switch off: with that capability flag set, MySQL reports matched rather than changed, so whatever is done here has to read the flag rather than pick one meaning.

Check what crates/protocol does with CLIENT_FOUND_ROWS today before choosing the fix.
