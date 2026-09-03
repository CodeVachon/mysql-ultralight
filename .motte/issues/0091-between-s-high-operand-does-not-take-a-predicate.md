---
id: 91
title: BETWEEN's high operand does not take a predicate
state: Todo
parent: 9
labels: [executor]
created: 2026-08-25T10:59:33Z
updated: 2026-09-03T14:01:17Z
---

## Description

Found by review during #0056, and pre-existing -- both constructs were refused outright before that commit.

MySQL's grammar makes BETWEEN's high operand a predicate, so a trailing IN, LIKE or REGEXP binds into it:

    SELECT 1 BETWEEN 0 AND 2 IN (2)   mysql 1   engine 0

Measured on 8.3. sqlparser gives (1 BETWEEN 0 AND 2) IN (2) and none of the corrections in crates/engine/src/expr.rs push it back, because every walk there lifts an operator out of a construct and this one has to push an operator in -- the opposite direction.

Recorded with the measurement in postfix_operands' doc comment. Worth noting the shape is exotic and nothing in the conformance corpus uses it, which is why it was left rather than fixed under #0056's renumbering.

## Notes

### 2026-09-03T14:01:17Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed, and it is not only IN: 1 BETWEEN 0 AND 2 IN (2), the same with LIKE '2', and the same with REGEXP '2' are all 1 on 8.3 and 0 here.
