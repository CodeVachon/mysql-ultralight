---
id: 198
title: CAST(<predicate> AS BINARY) answers the predicate exactly, not its result
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T16:15:47Z
updated: 2026-09-03T16:15:47Z
---

## Description

A deliberate trade made while fixing #0129/#0134/#0152, recorded here because it is a NEW wrong answer and the house rule is that a wrong answer is never acceptable silently.

The BINARY prefix must bind to its operand: 8.3 reads BINARY 'A' = 'a' as (BINARY 'A') = 'a'. sqlparser gives CAST('A' = 'a' AS BINARY) -- the prefix and the explicit cast produce the same AST, with no record of which spelling was written. So re-associating the prefix necessarily re-associates the explicit cast too.

    fixed by the trade (all measured wrong before):
      BINARY 'A' = 'a'      BINARY 'A' < 'a'      BINARY 'A' IN ('a')
      and the result type of BINARY 'A' LIKE 'a'
    cost by the trade:
      CAST('A' LIKE 'a' AS BINARY)   mysql the binary string '1'   engine 0

Four wrong answers fixed against one introduced, and the one introduced is a spelling nothing writes by choice. But it is still wrong, and it is invisible to the differential harness -- a corpus case would fail the empty baseline, so there is none.

Closing it properly needs the parser to keep which spelling produced the cast, i.e. a sqlparser change or a pre-parse token pass like the one #0094 used for EXTRACT's compound units. The reason for the trade is written above binary_prefix_over_predicate in crates/engine/src/expr.rs.
