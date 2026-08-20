---
id: 47
title: Integer columns accept values their width cannot hold
state: Done
parent: 9
labels: [executor, compat]
created: 2026-08-20T15:55:53Z
updated: 2026-08-20T16:36:46Z
---

## Description

Found while starting #0038. The engine stores 999 in a TINYINT, 99999 in a SMALLINT and 9999999999 in an INT; MySQL refuses all three with 1264 'Out of range value for column'. A FLOAT column likewise keeps full double precision where MySQL rounds to f32.

Wrong on its own - a test asserting that a column rejects an over-wide value passes here when it should not - and a blocker for #0038. Describing an INT as MYSQL_TYPE_LONG is only safe once the column cannot hold something wider: the binary protocol encodes by the declared type, so a TINYINT declared as MYSQL_TYPE_TINY holding 999 would be written as 999 as u8, and the client would read 231.

SqlType::coerce needs the per-width range from base_name and unsigned, erroring 1264, and TypeKind::Float needs to round to f32 when the declared type is FLOAT.

## Notes

### 2026-08-20T16:36:46Z — Christopher Vachon (user)

Done as part of #0038, because neither is safe alone. SqlType::coerce range-checks integers by declared width with 1264, matching MySQL including BOOLEAN (which shares TINYINT's range) and YEAR (1901 to 2155, plus zero). AUTO_INCREMENT goes through coerce too: a generated id is subject to the column's width like any other value, and a TINYINT AUTO_INCREMENT counting past 127 was stored and sent as one byte.

FLOAT was split out to #0048. Rounding the stored value to f32 makes the value right and the rendering worse - 1.2345678806304932 where MySQL prints 1.23457 - because Value::Float does not know which column it came from.
