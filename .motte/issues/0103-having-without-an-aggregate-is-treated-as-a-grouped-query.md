---
id: 103
title: HAVING without an aggregate is treated as a grouped query
state: Todo
parent: 9
labels: [executor]
created: 2026-08-25T14:48:58Z
updated: 2026-08-25T14:48:58Z
---

## Description

Found while implementing #0058, pre-existing and unrelated to it.

    SELECT b FROM J2 HAVING b > 1   mysql answers rows   engine 1055 ONLY_FULL_GROUP_BY

A HAVING with no aggregate and no GROUP BY does not group the query in MySQL; every column stays available and the clause filters rows. This engine treats the presence of HAVING as making the query grouped, so an ordinary column becomes a non-grouped reference and ONLY_FULL_GROUP_BY refuses it.

Refusing a statement MySQL answers, which is the direction that breaks working queries rather than merely disagreeing.
