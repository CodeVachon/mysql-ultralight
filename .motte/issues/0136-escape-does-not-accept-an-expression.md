---
id: 136
title: ESCAPE does not accept an expression
state: Todo
parent: 9
labels: [compat]
created: 2026-08-31T13:34:50Z
updated: 2026-09-03T14:03:11Z
---

## Description

Found while implementing #0093.

MySQL accepts any constant expression as LIKE's escape character and rejects only a column reference:

    'a%' LIKE 'a!%' ESCAPE (SELECT '!')   mysql 1      engine 1064
    ... ESCAPE CONCAT('!')                mysql 1      engine 1064
    ... ESCAPE CHAR(33)                   mysql 1      engine 1064
    ... ESCAPE e   (a column)             mysql 1210   engine 1064

sqlparser's parse_escape_char takes only a Value, so anything else fails at parse time. A refusal rather than a wrong answer, which is why #0093 left it.

The same parser-gap shape as #0094 (EXTRACT's compound units) and #0051 (infix MOD): the routes are a token-level rewrite or an upstream change. Note #0093 already measured what the semantics must be once it parses -- the value is converted to a string first and then length-checked, so ESCAPE 1 is the character '1' and ESCAPE 1.5 is 1210.

## Notes

### 2026-09-03T14:03:11Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed for the three expression forms: ESCAPE (SELECT '!'), ESCAPE CONCAT('!') and ESCAPE CHAR(33) all answer 1 on 8.3 and are 1064 here, refused by the parser before the engine sees them. The fourth case in the description (ESCAPE over a column, mysql 1210) was not re-measured.
