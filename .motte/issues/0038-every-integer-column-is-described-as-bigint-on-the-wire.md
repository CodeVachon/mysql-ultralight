---
id: 38
title: Every integer column is described as BIGINT on the wire
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T14:21:10Z
updated: 2026-08-20T14:21:10Z
---

## Description

Found by the differential harness. SqlType::column_type maps TypeKind::Int to ColumnType::LongLong at every width, so an INT column is described as MYSQL_TYPE_LONG by MySQL and MYSQL_TYPE_LONGLONG here. TINYINT, SMALLINT and MEDIUMINT are the same.

Sixty-eight of the harness's 103 divergences are column types, and this is the largest share: it fires on nearly every query that selects an integer column.

The declared type is already carried correctly - that was #0035. This is the wire mapping, which needs to branch on base_name the way numeric_precision already does.
