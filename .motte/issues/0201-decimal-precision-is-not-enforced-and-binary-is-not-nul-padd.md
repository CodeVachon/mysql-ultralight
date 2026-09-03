---
id: 201
title: DECIMAL precision is not enforced and BINARY is not NUL-padded
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T17:51:08Z
updated: 2026-09-03T17:51:08Z
---

## Description

Two declared-width gaps #0194/#0195 did not cover, both measured on 8.3.

    INSERT 999999999.99 INTO a DECIMAL(10,2)   mysql 1264 out of range   engine stores it
    BINARY(3) given 'a'                        mysql 0x610000 (NUL-padded)   engine 0x61

The first is the same class of wrong stored data #0194/#0195 fixed for strings and integers -- a DECIMAL's precision is a declared width like any other, and the target application declares 155 DECIMAL columns, which is why this is priority 2 rather than 3.

The second means a fixed-width binary column does not round-trip its declared width.
