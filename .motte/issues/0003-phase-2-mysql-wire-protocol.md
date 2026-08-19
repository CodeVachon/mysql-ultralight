---
id: 3
title: Phase 2 — MySQL wire protocol
state: Done
labels: [epic, phase-2]
blockedBy: [2]
created: 2026-08-18T14:10:54Z
updated: 2026-08-19T12:48:15Z
---

## Description

Server-side implementation of the MySQL 8 client/server protocol, sufficient for Prisma 5.4.2 to treat it as MySQL 8.3.

Must cover: connection handshake and capability negotiation, mysql_native_password auth (matching the --mysql-native-password=ON flag in the current docker-compose), the text protocol, the binary/prepared-statement protocol (Prisma leans on this heavily), result set metadata with correct column types and flags, OK/EOF/ERR packets with MySQL-compatible error codes and SQLSTATEs, multi-statement and multi-result handling, and both TCP and unix socket listeners.

## Notes

### 2026-08-19T12:48:15Z — Christopher Vachon (user)

Closed 2026-08-19. The wire protocol has been complete in practice for some time: handshake and capability negotiation, mysql_native_password with auth-switch, text and binary result sets, prepared statements with real column metadata, multi-statement commands with MORE_RESULTS_EXISTS, COM_SET_OPTION, unix sockets alongside TCP. Prisma, its migration engine and its client all speak to it without special cases. Tested at the packet level in crates/server/tests/wire.rs.
