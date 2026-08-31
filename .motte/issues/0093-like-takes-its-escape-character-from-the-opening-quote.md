---
id: 93
title: LIKE takes its escape character from the opening quote
state: Done
parent: 9
labels: [executor]
created: 2026-08-25T12:57:57Z
updated: 2026-08-31T14:42:57Z
---

## Description

Found by review during #0057, and pre-existing -- nothing in that change touches LIKE.

crates/engine/src/expr.rs's Expr::Like arm reads the escape character with

    escape_char.as_ref().and_then(|s| s.to_string().chars().next())

which takes the first character of the *rendered literal*, so it is the opening quote. Every ESCAPE clause therefore sets the escape character to a single quote, whatever was written.

    SELECT 'a%' LIKE 'a!%' ESCAPE '!'     mysql 1   engine 0
    SELECT 'a%' LIKE 'a''%' ESCAPE '!'    mysql 0   engine 1

The second is the decisive one: the engine can only answer 1 there if ' is acting as the escape character.

A wrong answer rather than a refusal, and silent -- LIKE returns a plausible 0 or 1 either way. The fix is to read the string's value rather than its Display rendering. Worth checking NOT LIKE and the same pattern anywhere else a literal is read back through to_string().

## Notes

### 2026-08-31T14:42:57Z — Christopher Vachon (user)

Done in 64f6ec3. ESCAPE now reads the literal's value through the existing literal(), not its Display rendering.

MySQL's rules around it are finer than the fix, all measured: an empty escape turns escaping off while a NULL one keeps the default backslash; the value is converted to a string before its length is checked, so ESCAPE 1 is the character '1' and ESCAPE 1.5 is 1210; a multi-character escape is 1210; and a binary escape is measured in bytes, so x'C3A9' is two characters and 1210 while x'21' is one. A single byte above ASCII names a byte this engine cannot match over chars, so it refuses (1235) rather than escaping with U+FFFD -- before the fix x'FF' and x'FE' were indistinguishable.

A second bug in the same arm: a trailing escape character failed the pattern where MySQL matches it literally.

Review caught the first cut checking the clause per row while its own comment claimed resolve time. Over an empty table, in an untaken CASE branch, and in ORDER BY, a malformed escape was silently accepted -- the same shape as #0058, where over zero rows nothing is evaluated. It is now refused at five resolve points and at prepare, after name resolution, because 8.3 answers 1054 before 1210. Two of the five were found by measuring rather than from the review's list.

The mutation proving that neuters all five resolve-time checks and leaves the per-row one intact: it fails the no-row test and passes the row test, and the pair is the claim. The earlier mutation only reordered two things inside eval, which proves ordering within a row and not that anything runs without one -- it is not claimed as coverage.

The corpus found an ESCAPE NULL nullability divergence on its first run, now fixed.

Filed rather than half-fixed: #0134 LIKE is case-sensitive where MySQL folds (and the engine's own = folds, so it disagrees with itself), #0135 NO_BACKSLASH_ESCAPES is not modelled at all, #0136 ESCAPE does not accept an expression.

The Display audit this issue asked for found two more live wrong answers: #0137 (INSERT reads its table name through Display, so INSERT INTO `my.table` is 1049 while CREATE and SELECT on the same name work) and #0138 (COLLATE keeps its backticks into information_schema and the image; nine sites double an embedded backtick). That makes four instances of one class -- #0058, #0059, this, and #0137.

14 mutations, 13 bite; one is a doc comment with nothing to mutate and says so. Corpus 1692 -> 1770, baseline empty, both gates re-run independently.
