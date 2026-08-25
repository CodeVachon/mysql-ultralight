---
id: 68
title: An ENUM in a numeric context is its ordinal
state: Done
parent: 9
labels: [executor]
created: 2026-08-21T13:36:26Z
updated: 2026-08-25T03:01:44Z
---

## Description

MySQL gives an ENUM a number: its 1-based position in the declared list. This engine stores the member as text and reads the number out of that text, so a member that is not itself numeric reads as 0.

    ENUM('a','b') holding 'a'          mysql   engine
    SELECT e + 0                            1        0
    SELECT -e                              -1       -0
    SELECT e = 1                            1        0
    SELECT CAST(e AS SIGNED)                1        0
    SELECT e = 'a'                          1        1

Only the comparison against the *member* behaves; every numeric reading is wrong, comparison included.

Found while fixing #0063, where unary minus started reading its operand as a number instead of answering NULL. That made the divergence visible rather than causing it -- before, these answered NULL, which was wrong too.

The same shape as #0054, where a temporal loses its type on the way into arithmetic: crates/engine/src/value.rs holds only the text, and the answer depends on the column's declared type. SqlType already carries the variants in declaration order, so the ordinal is known -- it is the value that cannot reach it.

## Notes

### 2026-08-25T03:01:44Z — Christopher Vachon (user)

Done in 4e3b630. Value::Enum { member, ordinal }, tagged in coerce where the declared variants are already in hand. FORMAT_VERSION 6 -> 7, tag 9 appended.

ENUM('7','8','9') holding '7' is the case that settles ordinal-beats-text: MySQL says n + 0 is 1 and n = 7 is 0, where reading the text says 7 and 1.

Four things the suggested design got wrong, all found by measuring:

Sorting and comparison disagree. ORDER BY is by ordinal -- ENUM('b','a') sorts b before a -- while <, =, MIN and MAX go by the member. compare could not be reused; sort_cmp is used only by compare_keys.

The ordinal does not survive every expression. IF, CASE, NULLIF and a merged derived table keep it; COALESCE, IFNULL, GREATEST, LEAST, MIN, MAX and a scalar subquery drop it, single-argument COALESCE included.

A set operation drops it hardest. UNION reports VAR_STRING and orders by the member even when both sides are the same declaration, and x + 0 over the result is 0. That is simpler than untagging where declarations differ, and it removes the only construct that could put two incomparable ordinals in one sort key. Still out of reach: IF(c, e1, e2) across two declarations, which MySQL answers by the chosen argument's own ordinal, so the engine already gives the only available answer.

An ENUM is approximate like any string, so -e and SUM(e) are DOUBLE not NEWDECIMAL. Typing that generally exposed a separate bug in approximate_total: it read operands with a whole-string parse where a row reads a leading number, so SUM over '12abc' was 0 beside a row answering 12, and SUM over any BLOB was 0 whatever it held -- as_float has no Bytes arm. MySQL reads a BLOB exactly as it reads text.

Assignment follows MySQL's rule: a number is always an index, a string matches a member first and only then parses as an index, matching is case-insensitive and stores the declared spelling, a float truncates toward zero, and '0' is the error member.

44 mutations. Three did not bite and each was handled rather than papered over: to_wire needed a wire-encoding assertion; the VAR_STRING exclusion needed a SUM((SELECT n ...)) counterweight proving an unknown expression stays exact; and one set-operation assertion had to move off a derived table, whose outer projection re-derives the type and hid what the operation itself reported. Two are inert by construction and say so.

Corpus 418 -> 510, baseline empty.
