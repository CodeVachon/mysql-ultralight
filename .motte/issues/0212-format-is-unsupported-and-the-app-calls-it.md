---
id: 212
title: FORMAT() is unsupported, and the app calls it
state: Todo
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-03T23:20:40Z
updated: 2026-09-03T23:20:40Z
---

## Description

Found while implementing #0118/#0122/#0123.

    SELECT FORMAT(1/3, 9)
    mysql   0.333333333
    engine  ERROR 1235 does not yet support function 'format()'

Worth noting for whoever implements it: FORMAT reads the decimal's VALUE, unlike ROUND and TRUNCATE which cap at the argument's shown scale. That distinction is now modelled (see decimal.rs and functions.rs::shown_places), so the information is there to do this correctly.

Priority 2 because the conformance scan counts 9 FORMAT calls in application-mono-repo.
