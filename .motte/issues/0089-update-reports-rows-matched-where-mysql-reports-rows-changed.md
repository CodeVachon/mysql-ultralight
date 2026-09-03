---
id: 89
title: UPDATE reports rows matched where MySQL reports rows changed
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-25T03:02:14Z
updated: 2026-09-03T15:34:24Z
---

## Description

Found by review during #0068, via a corpus case that was then removed rather than blessed.

UPDATE t SET x = x over rows that already hold that value reports every row as affected; MySQL reports 0, because it counts rows actually changed rather than rows matched. The distinction is visible to any client that reads affected_rows -- Prisma among them, which uses the count to decide whether an update found its target.

The engine writes the value unconditionally and counts the write. Making it right means comparing the new value against the old before counting, which is also what CLIENT_FOUND_ROWS exists to switch off: with that capability flag set, MySQL reports matched rather than changed, so whatever is done here has to read the flag rather than pick one meaning.

Check what crates/protocol does with CLIENT_FOUND_ROWS today before choosing the fix.

## Notes

### 2026-09-03T14:01:17Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed via SELECT ROW_COUNT() after each UPDATE (the client prints nothing for a bare UPDATE, so a probe without ROW_COUNT compares nothing and falsely reads as agreement). UPDATE U SET x=x over two matching rows: mysql 0, engine 2. Same for x=x+0. A real change reports 2 on both.

### 2026-09-03T14:21:05Z — Christopher Vachon (user)

APP PRIORITY 1 for application-mono-repo. The description already names the reason and the target confirms it: Prisma reads affected_rows to decide whether an update found its target, so an UPDATE that changes nothing reports 2 here and 0 on MySQL. The app has 57 multi-table UPDATE sites plus every Prisma .update()/.updateMany() call, and updateMany returns that count to application code.

### 2026-09-03T15:12:43Z — Christopher Vachon (user)

PRIORITY CORRECTION, and it is a correction to my own analysis rather than to the issue.

I labelled this app-priority-1 on the reasoning that 'Prisma reads affected_rows to decide whether an update found its target'. That is true, but it does not make the engine wrong for Prisma. Measured independently by reading the capability line off the engine's own --trace handshake while a real Prisma client connected:

    capabilities: LONG_PASSWORD FOUND_ROWS CONNECT_WITH_DB PROTOCOL_41 ...

**Prisma sets CLIENT_FOUND_ROWS.** With that flag, MySQL reports MATCHED -- which is exactly what this engine already reported. So for the target application's own client the engine was already agreeing, and the divergence bit only clients that leave the flag off: the mysql CLI, the differential harness's mysql crate, and raw drivers.

The fix is still right and still worth having -- the engine now reads the flag instead of hard-coding one meaning, so it is correct for both kinds of client rather than accidentally correct for one. But the app-facing urgency was lower than I claimed. Re-labelled app-priority-2.

Noting it here because the same mistake is easy to repeat: 'the client reads this value' is not the same as 'the client disagrees about this value'.

### 2026-09-03T15:34:24Z — Christopher Vachon (user)

Landed in 0cb3610, gate green on main (including the Prisma end-to-end example, which is the one that would notice). Differential 3706 statements, 0 divergences.
