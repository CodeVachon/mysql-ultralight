---
id: 224
title: LENGTH over non-UTF-8 bytes counts the replacement characters
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-04T04:08:08Z
updated: 2026-09-04T04:08:08Z
---

## Description

Found while implementing #0212/#0215, pre-existing and reproducible with no cast involved.

    SELECT LENGTH(UNHEX('FFEE')), OCTET_LENGTH(UNHEX('FFEE'))
    mysql   2  2      engine  6  6
    SELECT LENGTH(UNHEX('C3A9C3')), CHAR_LENGTH(UNHEX('C3A9C3'))
    mysql   3  3      engine  5  2

Cause: those arms read as_text(), which decodes a Value::Bytes lossily, so each invalid byte becomes a 3-byte U+FFFD and is then counted.

Same root as #0145 (a string function mangles non-UTF-8 bytes through a lossy decode), which is still open -- recording here because LENGTH and OCTET_LENGTH are a second family of readers beyond the ones #0145 lists, and because the target declares no binary columns so both stay priority 3.
