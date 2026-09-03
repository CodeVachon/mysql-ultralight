---
id: 95
title: SET accepts values MySQL refuses or clamps
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T13:13:52Z
updated: 2026-09-03T14:03:11Z
---

## Description

Measured during #0057, which fixed default_week_format only. The rest is untouched.

    SET max_allowed_packet = 'abc'    mysql 1232   engine accepts silently
    SET sql_mode = 'NOPE'             mysql 1231   engine accepts silently
    SET max_execution_time = -5       mysql clamps to 0    engine stores -5
    SET group_concat_max_len = 3      mysql clamps up to 4 engine stores 3

MySQL's rule for an integer variable, measured on default_week_format: an out-of-range value clamps with warning 1292 rather than erroring, while a non-integer raises 1232 -- and that includes a string spelling a number, so '3', 3.7 and NULL all raise where TRUE gives 1 and 1 + 1 gives 2.

#0057 implemented exactly that for default_week_format. The same rule needs applying to every other variable the engine accepts, with each one's own bounds read from the server rather than guessed.

Related, same area: an overridden variable reads back as VAR_STRING where MySQL sends its real type -- @@default_week_format is UNSIGNED LONGLONG there -- which is why SELECT @@default_week_format is deliberately absent from the differential corpus.

## Notes

### 2026-09-03T14:03:11Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3. Three of the four claims hold; one is STALE, and a new one turned up.

    SET max_allowed_packet = 'abc'   mysql 1232   engine accepts silently   HOLDS
    SET sql_mode = 'NOPE'            mysql 1231   engine accepts silently   HOLDS
    SET max_execution_time = -5      mysql 0      engine -5                 HOLDS
    SET group_concat_max_len = 3     mysql 4      engine 4                  STALE -- the engine clamps correctly now

New, found while checking the last one: a system variable is the wrong TYPE here.
    SELECT @@group_concat_max_len       renders right-aligned (a number) on 8.3, left-aligned (a string) here
    SELECT @@group_concat_max_len + 1   mysql LONGLONG   engine DOUBLE

So the clamping half of this issue is smaller than written, and there is a separate typing problem behind it.
