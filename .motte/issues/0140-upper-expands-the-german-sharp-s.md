---
id: 140
title: UPPER expands the German sharp s
state: Done
parent: 9
labels: [executor]
created: 2026-08-31T20:28:05Z
updated: 2026-09-02T18:44:53Z
---

## Description

Found through HEX(UPPER('straße')) while implementing #0130.

    SELECT UPPER('straße')   mysql STRAßE   engine STRASSE

Rust's str::to_uppercase performs the full Unicode uppercase mapping, which expands U+00DF to SS. MySQL's utf8mb4 case folding is a per-character table and leaves it alone.

A wrong answer, and a length change as well -- LENGTH and CHAR_LENGTH of the result differ, so it is visible beyond the characters themselves.

Worth checking LOWER in the same pass, and the other characters whose Unicode uppercase mapping is multi-character (U+FB00-U+FB06 ligatures, U+0149, U+01F0, the Greek iota-subscript forms). The fix is a per-character mapping rather than to_uppercase, which also matters for #0129 once per-column charsets exist.

## Notes

### 2026-09-02T18:44:53Z — Christopher Vachon (user)

Done in ef0c031.

Swept all 1112064 codepoints in both directions against 8.3 rather than hand-picking. The issue named one character; the engine broke 396 -- and the rule is not 'decline a mapping that expands', because 28 characters go the other way, where Unicode expands and MySQL folds to one character (27 Greek iota-subscript forms and İ). Most of the rest is that MySQL's tables stop at Unicode 9.0.

So MySQL's own utf8mb4_0900_ai_ci table is transcribed, run-length compressed, rather than patched. An exception list over to_uppercase measures a fifth of the size and was rejected on failure mode, not size: it is correct only relative to the compiler's Unicode version, so a future Rust learning one more mapping reopens the bug silently with nothing changed in the tree.

The case table belongs to the COLLATION, not the character set. utf8mb4 has two on 8.3 and they disagree on 616 characters upward and 608 downward; a column takes 0900_ai_ci and a literal takes the connection's, which clients negotiate as general_ci. This encodes the column's; the literal's is #0154.

Nineteen characters fold wider in UTF-8, and MySQL folds in place into a buffer sized to the argument, so it writes over unread bytes and stops mid-string -- UPPER('aȿb') is AȾ on 8.3, and the b is gone. Review flagged reproducing that as data loss for literals, and the measurement settled it: over 304 strings, reproducing the truncation and declining those folds are wrong in exactly 152 cases each, mirror images -- but one deletes 266 characters and the other deletes none. The tie on accuracy is what makes it clean. No answer is now shorter than its argument, and declining is also general_ci's answer, so those nineteen could finally be asked as literals in the corpus.

Refusing them was considered and rejected: the check sits in the per-character path, so UPPER(col) would succeed over an empty table and raise over a populated one -- #0009's zero-rows pattern.

The engine is knowingly wrong for those nineteen over a column, recorded in four places with 8.3's answers beside them (#0154).

U+A7F5/U+A7F6 was challenged in review as the one Unicode 9.0 pair missing from the tables. Measured: 8.3 leaves it alone under both collations, so the transcription is right -- and a comment now says so, since the stated rule does not predict it while Osage and Adlam, also 9.0, are present.

9 mutations, all bite. Corpus 2420 -> 2454, baseline empty, both gates verified independently.

Filed: #0154 (a literal's fold uses the wrong collation), #0155 (a supplementary character survives in a generated column name).
