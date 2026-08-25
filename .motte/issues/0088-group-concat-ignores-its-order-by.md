---
id: 88
title: GROUP_CONCAT ignores its ORDER BY
state: Todo
parent: 9
labels: [executor]
created: 2026-08-25T03:02:14Z
updated: 2026-08-25T03:02:14Z
---

## Description

Found by review during #0068, outside that change.

GROUP_CONCAT(x ORDER BY y) concatenates in whatever order the rows arrive; the ORDER BY inside the call is parsed and discarded. MySQL orders the concatenated values by it.

A wrong answer rather than a refusal, and a quiet one -- the result is a plausible string that happens to be in the wrong order, and a caller comparing it against an expected value fails without any indication that the ordering clause was the part ignored. Refusing the clause would at least be honest until it is implemented.

Worth checking SEPARATOR and DISTINCT in the same pass, since they are parsed at the same site, plus group_concat_max_len.
