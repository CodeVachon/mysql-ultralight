---
id: 188
title: Integer functions with a narrow max_length are materialised wide
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T12:52:56Z
updated: 2026-09-03T12:52:56Z
---

## Description

Found while implementing #0110, which modelled the width for literals, arithmetic and column references but not for functions.

Through a FROM-less view on 8.3, all of these are int and all are bigint here:

    MONTH DAY WEEK MINUTE SECOND        max_length 3
    DAYOFWEEK WEEKDAY QUARTER STRCMP    2
    DAYOFYEAR HOUR                      4
    ASCII FIELD                         3
    TO_DAYS                             8
    DATEDIFF                            9
    EXTRACT(YEAR ...)                   5
    GREATEST LEAST IF NULLIF COALESCE CASE INTERVAL (SELECT 1) 1 IN (1,2) TRUE   1-2

MIN/MAX and IFNULL are bigint on 8.3 despite narrow widths, because they override tmp_table_field -- so this is a per-function table, not a rule that can be derived.

Same mechanism as #0110 (Item::max_length, <=9 characters narrows to int); this is the set of items whose width the engine does not model yet.
