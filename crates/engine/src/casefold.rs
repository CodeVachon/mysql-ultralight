//! `UPPER` and `LOWER`, which are neither Unicode's case mapping nor one table.
//!
//! Rust's `str::to_uppercase` is Unicode's *full* mapping, so it may answer a
//! sequence: `ß` becomes `SS`, `ﬁ` becomes `FI`, `ǰ` becomes `J` plus a
//! combining caron, `İ` lowercases to `i` plus a combining dot. MySQL's is a
//! per-character table — one character in, exactly one character or itself
//! out, never a sequence — so `UPPER('straße')` is `STRAßE` there and was
//! `STRASSE` here. A wrong answer, and a length change with it: `LENGTH` and
//! `CHAR_LENGTH` of the result diverged too. #0140.
//!
//! Everything here was measured by folding all 1,112,064 codepoints on 8.3 in
//! both directions and diffing `HEX()` of the answers.
//!
//! ## The table belongs to the collation, not the character set
//!
//! The part worth knowing before touching this file. utf8mb4 has two case
//! tables on 8.3 and they disagree on 616 characters upward and 608 downward:
//!
//! | | `utf8mb4_0900_ai_ci` | `utf8mb4_general_ci` |
//! |---|---|---|
//! | characters `UPPER` changes | 1321 | 706 |
//! | characters `LOWER` changes | 1304 | 696 |
//! | anything above the BMP | Deseret, Adlam, Vithkuqi, … | never |
//! | `HEX(UPPER('ȿ'))` | `''` — see below | `C8BF` |
//! | `HEX(LOWER('ẞ'))` | `C39F` | `E1BA9E` |
//!
//! `utf8mb4_0900_ai_ci` is the one encoded here. It is what a column declared
//! without a `COLLATE` clause gets on 8.3 — so it is the collation of every
//! column Prisma creates, and the one that folds in `WHERE UPPER(col) = ?` —
//! and it is what this engine's own `information_schema.COLLATIONS` reports as
//! its default.
//!
//! It is *not* what a string literal folds under. MySQL takes a literal's
//! collation from the connection, and the drivers negotiate 45,
//! `utf8mb4_general_ci`, which is why `SELECT UPPER('𐐨')` differs from
//! `SELECT UPPER(col)` over the same character on the same server. This engine
//! has one table for both. Both halves are measured and the split is #0129's
//! and #0152's to close; the differential corpus asks the characters where the
//! two collations disagree through a *column*, where this table is the one
//! MySQL uses too.
//!
//! ## Why the whole table rather than Rust's with patches
//!
//! Both were measured before choosing. Rust's mapping restricted to its
//! single-character answers agrees with `utf8mb4_0900_ai_ci` on 1,111,853
//! codepoints in each direction and disagrees on 396 — 211 upward and 185
//! downward. Of those, 368 are characters that gained a case mapping after
//! Unicode 9.0, where 8.3's tables stop, and the other 28 are the 27 Greek
//! iota-subscript forms plus `İ`, whose full mapping is a sequence where 8.3
//! has one character. The nineteen widening folds below are *not* among them:
//! the tables give those Rust's answer, and what differs is what `fold` does
//! with it. As ranges the 396 exceptions are 69 lines against the 354 below,
//! so the table is five times the source.
//!
//! It is here anyway because of how the two fail. A patch list is correct
//! relative to the compiler's Unicode version: a future Rust learning one more
//! mapping reopens this bug silently, with nothing in the tree changed. A
//! table that says what 8.3 says can only be wrong about 8.3.
//!
//! ## A fold that would grow is left alone, and 8.3 truncates
//!
//! Nineteen of the mappings below are *longer in UTF-8* than the character
//! they replace — `ȿ ɀ ɐ ɑ ɒ ɜ ɡ ɥ ɦ ɪ ɫ ɬ ɱ ɽ ʇ ʝ ʞ` upward and `Ⱥ Ⱦ`
//! downward. MySQL folds in place: `Item_str_conv::val_str` hands the
//! charset's fold one buffer as both source and destination, sized to the
//! argument, because for utf8mb4 the multiplier that would widen it is 1. Such
//! a mapping writes over bytes that have not been read yet, the next read
//! lands mid-character, and the scan stops — so 8.3 answers a *truncated*
//! string, measured:
//!
//! ```text
//! HEX(UPPER('ȿ'))     8.3 over a column: ''          two bytes in, three needed
//! HEX(UPPER('aȿb'))   8.3 over a column: 41E2B1BE    `A` `Ⱦ` fills four bytes; the `b` is gone
//! HEX(UPPER('ȿaa'))   8.3 over a column: E2B1BE      `Ⱦ` wrote over the first `a`
//! ```
//!
//! `fold` deliberately does **not** reproduce that. It leaves a character
//! alone when its fold would be wider, which is what `utf8mb4_general_ci` does
//! — that table has no entry for any of the nineteen — and therefore exactly
//! what 8.3 answers for a *literal*.
//!
//! The choice was measured rather than argued. Over 304 strings built round
//! the nineteen, in both directions and eight contexts, reproducing the
//! truncation and declining to are **equally wrong — 152 of 304 each** — one
//! against a literal and the other against a column. They are not equally
//! bad. Reproducing it deletes 266 characters across those 152; declining to
//! deletes none, because no answer here is ever shorter than its argument:
//!
//! ```text
//! UPPER('aȿb')   8.3 column  AȾ    8.3 literal  AȿB    this engine  AȿB
//! ```
//!
//! So the engine is wrong for these nineteen over a *column*, where it answers
//! the unfolded character and 8.3 truncates. That is #0154, and it is the
//! direction to be wrong in: `UPDATE t SET c = UPPER(c)` cannot lose bytes a
//! caller handed it, and a test double that silently drops the tail of a
//! string is a worse failure than one that leaves an IPA letter unfolded.
//!
//! Refusing was considered and rejected. The check would have to sit in the
//! per-character path, so `UPPER(col)` would succeed over an empty table and
//! raise over a populated one — #0009's zero-rows pattern, which this epic has
//! now found three times.

/// `UPPER` under `utf8mb4_0900_ai_ci`, as `(first, last, step, first's
/// answer)`: every `step`-th character from `first` through `last` maps to
/// itself plus the same offset, so a character in a run answers `first's
/// answer` plus its distance from `first`.
///
/// `step` is 2 for the blocks that alternate capital and small — Latin
/// Extended-A pairs them off that way — and 1 everywhere else. Sorted by
/// `first` and non-overlapping, which `mapped` relies on and a test holds it
/// to. The trailing comments name the script a run starts in, not its
/// contents.
const UPPER: &[(char, char, u32, char)] = &[
    ('\u{0061}', '\u{007A}', 1, '\u{0041}'), // Latin
    ('\u{00B5}', '\u{00B5}', 1, '\u{039C}'), // Micro
    ('\u{00E0}', '\u{00F6}', 1, '\u{00C0}'), // Latin
    ('\u{00F8}', '\u{00FE}', 1, '\u{00D8}'),
    ('\u{00FF}', '\u{00FF}', 1, '\u{0178}'),
    ('\u{0101}', '\u{012F}', 2, '\u{0100}'),
    ('\u{0131}', '\u{0131}', 1, '\u{0049}'),
    ('\u{0133}', '\u{0137}', 2, '\u{0132}'),
    ('\u{013A}', '\u{0148}', 2, '\u{0139}'),
    ('\u{014B}', '\u{0177}', 2, '\u{014A}'),
    ('\u{017A}', '\u{017E}', 2, '\u{0179}'),
    ('\u{017F}', '\u{017F}', 1, '\u{0053}'),
    ('\u{0180}', '\u{0180}', 1, '\u{0243}'),
    ('\u{0183}', '\u{0185}', 2, '\u{0182}'),
    ('\u{0188}', '\u{0188}', 1, '\u{0187}'),
    ('\u{018C}', '\u{018C}', 1, '\u{018B}'),
    ('\u{0192}', '\u{0192}', 1, '\u{0191}'),
    ('\u{0195}', '\u{0195}', 1, '\u{01F6}'),
    ('\u{0199}', '\u{0199}', 1, '\u{0198}'),
    ('\u{019A}', '\u{019A}', 1, '\u{023D}'),
    ('\u{019E}', '\u{019E}', 1, '\u{0220}'),
    ('\u{01A1}', '\u{01A5}', 2, '\u{01A0}'),
    ('\u{01A8}', '\u{01A8}', 1, '\u{01A7}'),
    ('\u{01AD}', '\u{01AD}', 1, '\u{01AC}'),
    ('\u{01B0}', '\u{01B0}', 1, '\u{01AF}'),
    ('\u{01B4}', '\u{01B6}', 2, '\u{01B3}'),
    ('\u{01B9}', '\u{01B9}', 1, '\u{01B8}'),
    ('\u{01BD}', '\u{01BD}', 1, '\u{01BC}'),
    ('\u{01BF}', '\u{01BF}', 1, '\u{01F7}'),
    ('\u{01C5}', '\u{01C5}', 1, '\u{01C4}'),
    ('\u{01C6}', '\u{01C6}', 1, '\u{01C4}'),
    ('\u{01C8}', '\u{01C8}', 1, '\u{01C7}'),
    ('\u{01C9}', '\u{01C9}', 1, '\u{01C7}'),
    ('\u{01CB}', '\u{01CB}', 1, '\u{01CA}'),
    ('\u{01CC}', '\u{01CC}', 1, '\u{01CA}'),
    ('\u{01CE}', '\u{01DC}', 2, '\u{01CD}'),
    ('\u{01DD}', '\u{01DD}', 1, '\u{018E}'),
    ('\u{01DF}', '\u{01EF}', 2, '\u{01DE}'),
    ('\u{01F2}', '\u{01F2}', 1, '\u{01F1}'),
    ('\u{01F3}', '\u{01F3}', 1, '\u{01F1}'),
    ('\u{01F5}', '\u{01F5}', 1, '\u{01F4}'),
    ('\u{01F9}', '\u{021F}', 2, '\u{01F8}'),
    ('\u{0223}', '\u{0233}', 2, '\u{0222}'),
    ('\u{023C}', '\u{023C}', 1, '\u{023B}'),
    ('\u{023F}', '\u{0240}', 1, '\u{2C7E}'),
    ('\u{0242}', '\u{0242}', 1, '\u{0241}'),
    ('\u{0247}', '\u{024F}', 2, '\u{0246}'),
    ('\u{0250}', '\u{0250}', 1, '\u{2C6F}'),
    ('\u{0251}', '\u{0251}', 1, '\u{2C6D}'),
    ('\u{0252}', '\u{0252}', 1, '\u{2C70}'),
    ('\u{0253}', '\u{0253}', 1, '\u{0181}'),
    ('\u{0254}', '\u{0254}', 1, '\u{0186}'),
    ('\u{0256}', '\u{0257}', 1, '\u{0189}'),
    ('\u{0259}', '\u{0259}', 1, '\u{018F}'),
    ('\u{025B}', '\u{025B}', 1, '\u{0190}'),
    ('\u{025C}', '\u{025C}', 1, '\u{A7AB}'),
    ('\u{0260}', '\u{0260}', 1, '\u{0193}'),
    ('\u{0261}', '\u{0261}', 1, '\u{A7AC}'),
    ('\u{0263}', '\u{0263}', 1, '\u{0194}'),
    ('\u{0265}', '\u{0265}', 1, '\u{A78D}'),
    ('\u{0266}', '\u{0266}', 1, '\u{A7AA}'),
    ('\u{0268}', '\u{0268}', 1, '\u{0197}'),
    ('\u{0269}', '\u{0269}', 1, '\u{0196}'),
    ('\u{026A}', '\u{026A}', 1, '\u{A7AE}'),
    ('\u{026B}', '\u{026B}', 1, '\u{2C62}'),
    ('\u{026C}', '\u{026C}', 1, '\u{A7AD}'),
    ('\u{026F}', '\u{026F}', 1, '\u{019C}'),
    ('\u{0271}', '\u{0271}', 1, '\u{2C6E}'),
    ('\u{0272}', '\u{0272}', 1, '\u{019D}'),
    ('\u{0275}', '\u{0275}', 1, '\u{019F}'),
    ('\u{027D}', '\u{027D}', 1, '\u{2C64}'),
    ('\u{0280}', '\u{0280}', 1, '\u{01A6}'),
    ('\u{0283}', '\u{0283}', 1, '\u{01A9}'),
    ('\u{0287}', '\u{0287}', 1, '\u{A7B1}'),
    ('\u{0288}', '\u{0288}', 1, '\u{01AE}'),
    ('\u{0289}', '\u{0289}', 1, '\u{0244}'),
    ('\u{028A}', '\u{028B}', 1, '\u{01B1}'),
    ('\u{028C}', '\u{028C}', 1, '\u{0245}'),
    ('\u{0292}', '\u{0292}', 1, '\u{01B7}'),
    ('\u{029D}', '\u{029D}', 1, '\u{A7B2}'),
    ('\u{029E}', '\u{029E}', 1, '\u{A7B0}'),
    ('\u{0345}', '\u{0345}', 1, '\u{0399}'), // Combining
    ('\u{0371}', '\u{0373}', 2, '\u{0370}'), // Greek
    ('\u{0377}', '\u{0377}', 1, '\u{0376}'),
    ('\u{037B}', '\u{037D}', 1, '\u{03FD}'),
    ('\u{03AC}', '\u{03AC}', 1, '\u{0386}'),
    ('\u{03AD}', '\u{03AF}', 1, '\u{0388}'),
    ('\u{03B1}', '\u{03C1}', 1, '\u{0391}'),
    ('\u{03C2}', '\u{03C2}', 1, '\u{03A3}'),
    ('\u{03C3}', '\u{03CB}', 1, '\u{03A3}'),
    ('\u{03CC}', '\u{03CC}', 1, '\u{038C}'),
    ('\u{03CD}', '\u{03CE}', 1, '\u{038E}'),
    ('\u{03D0}', '\u{03D0}', 1, '\u{0392}'),
    ('\u{03D1}', '\u{03D1}', 1, '\u{0398}'),
    ('\u{03D5}', '\u{03D5}', 1, '\u{03A6}'),
    ('\u{03D6}', '\u{03D6}', 1, '\u{03A0}'),
    ('\u{03D7}', '\u{03D7}', 1, '\u{03CF}'),
    ('\u{03D9}', '\u{03EF}', 2, '\u{03D8}'),
    ('\u{03F0}', '\u{03F0}', 1, '\u{039A}'),
    ('\u{03F1}', '\u{03F1}', 1, '\u{03A1}'),
    ('\u{03F2}', '\u{03F2}', 1, '\u{03F9}'),
    ('\u{03F3}', '\u{03F3}', 1, '\u{037F}'),
    ('\u{03F5}', '\u{03F5}', 1, '\u{0395}'),
    ('\u{03F8}', '\u{03F8}', 1, '\u{03F7}'),
    ('\u{03FB}', '\u{03FB}', 1, '\u{03FA}'),
    ('\u{0430}', '\u{044F}', 1, '\u{0410}'), // Cyrillic
    ('\u{0450}', '\u{045F}', 1, '\u{0400}'),
    ('\u{0461}', '\u{0481}', 2, '\u{0460}'),
    ('\u{048B}', '\u{04BF}', 2, '\u{048A}'),
    ('\u{04C2}', '\u{04CE}', 2, '\u{04C1}'),
    ('\u{04CF}', '\u{04CF}', 1, '\u{04C0}'),
    ('\u{04D1}', '\u{052F}', 2, '\u{04D0}'),
    ('\u{0561}', '\u{0586}', 1, '\u{0531}'), // Armenian
    ('\u{13F8}', '\u{13FD}', 1, '\u{13F0}'), // Cherokee
    ('\u{1C80}', '\u{1C80}', 1, '\u{0412}'), // Cyrillic
    ('\u{1C81}', '\u{1C81}', 1, '\u{0414}'),
    ('\u{1C82}', '\u{1C82}', 1, '\u{041E}'),
    ('\u{1C83}', '\u{1C84}', 1, '\u{0421}'),
    ('\u{1C85}', '\u{1C85}', 1, '\u{0422}'),
    ('\u{1C86}', '\u{1C86}', 1, '\u{042A}'),
    ('\u{1C87}', '\u{1C87}', 1, '\u{0462}'),
    ('\u{1C88}', '\u{1C88}', 1, '\u{A64A}'),
    ('\u{1D79}', '\u{1D79}', 1, '\u{A77D}'), // Latin
    ('\u{1D7D}', '\u{1D7D}', 1, '\u{2C63}'),
    ('\u{1E01}', '\u{1E95}', 2, '\u{1E00}'),
    ('\u{1E9B}', '\u{1E9B}', 1, '\u{1E60}'),
    ('\u{1EA1}', '\u{1EFF}', 2, '\u{1EA0}'),
    ('\u{1F00}', '\u{1F07}', 1, '\u{1F08}'), // Greek
    ('\u{1F10}', '\u{1F15}', 1, '\u{1F18}'),
    ('\u{1F20}', '\u{1F27}', 1, '\u{1F28}'),
    ('\u{1F30}', '\u{1F37}', 1, '\u{1F38}'),
    ('\u{1F40}', '\u{1F45}', 1, '\u{1F48}'),
    ('\u{1F51}', '\u{1F57}', 2, '\u{1F59}'),
    ('\u{1F60}', '\u{1F67}', 1, '\u{1F68}'),
    ('\u{1F70}', '\u{1F71}', 1, '\u{1FBA}'),
    ('\u{1F72}', '\u{1F75}', 1, '\u{1FC8}'),
    ('\u{1F76}', '\u{1F77}', 1, '\u{1FDA}'),
    ('\u{1F78}', '\u{1F79}', 1, '\u{1FF8}'),
    ('\u{1F7A}', '\u{1F7B}', 1, '\u{1FEA}'),
    ('\u{1F7C}', '\u{1F7D}', 1, '\u{1FFA}'),
    ('\u{1F80}', '\u{1F87}', 1, '\u{1F88}'),
    ('\u{1F90}', '\u{1F97}', 1, '\u{1F98}'),
    ('\u{1FA0}', '\u{1FA7}', 1, '\u{1FA8}'),
    ('\u{1FB0}', '\u{1FB1}', 1, '\u{1FB8}'),
    ('\u{1FB3}', '\u{1FB3}', 1, '\u{1FBC}'),
    ('\u{1FBE}', '\u{1FBE}', 1, '\u{0399}'),
    ('\u{1FC3}', '\u{1FC3}', 1, '\u{1FCC}'),
    ('\u{1FD0}', '\u{1FD1}', 1, '\u{1FD8}'),
    ('\u{1FE0}', '\u{1FE1}', 1, '\u{1FE8}'),
    ('\u{1FE5}', '\u{1FE5}', 1, '\u{1FEC}'),
    ('\u{1FF3}', '\u{1FF3}', 1, '\u{1FFC}'),
    ('\u{214E}', '\u{214E}', 1, '\u{2132}'), // Turned
    ('\u{2170}', '\u{217F}', 1, '\u{2160}'), // Small
    ('\u{2184}', '\u{2184}', 1, '\u{2183}'), // Latin
    ('\u{24D0}', '\u{24E9}', 1, '\u{24B6}'), // Circled
    ('\u{2C30}', '\u{2C5E}', 1, '\u{2C00}'), // Glagolitic
    ('\u{2C61}', '\u{2C61}', 1, '\u{2C60}'), // Latin
    ('\u{2C65}', '\u{2C65}', 1, '\u{023A}'),
    ('\u{2C66}', '\u{2C66}', 1, '\u{023E}'),
    ('\u{2C68}', '\u{2C6C}', 2, '\u{2C67}'),
    ('\u{2C73}', '\u{2C73}', 1, '\u{2C72}'),
    ('\u{2C76}', '\u{2C76}', 1, '\u{2C75}'),
    ('\u{2C81}', '\u{2CE3}', 2, '\u{2C80}'), // Coptic
    ('\u{2CEC}', '\u{2CEE}', 2, '\u{2CEB}'),
    ('\u{2CF3}', '\u{2CF3}', 1, '\u{2CF2}'),
    ('\u{2D00}', '\u{2D25}', 1, '\u{10A0}'), // Georgian
    ('\u{2D27}', '\u{2D27}', 1, '\u{10C7}'),
    ('\u{2D2D}', '\u{2D2D}', 1, '\u{10CD}'),
    ('\u{A641}', '\u{A66D}', 2, '\u{A640}'), // Cyrillic
    ('\u{A681}', '\u{A69B}', 2, '\u{A680}'),
    ('\u{A723}', '\u{A72F}', 2, '\u{A722}'), // Latin
    ('\u{A733}', '\u{A76F}', 2, '\u{A732}'),
    ('\u{A77A}', '\u{A77C}', 2, '\u{A779}'),
    ('\u{A77F}', '\u{A787}', 2, '\u{A77E}'),
    ('\u{A78C}', '\u{A78C}', 1, '\u{A78B}'),
    ('\u{A791}', '\u{A793}', 2, '\u{A790}'),
    ('\u{A797}', '\u{A7A9}', 2, '\u{A796}'),
    // Page A7 stops here, and `U+A7F6` -> `U+A7F5` is the one gap the "Unicode
    // 9.0" rule above does not predict: that pair *is* Unicode 9.0, as are
    // Osage and Adlam below, which 8.3 does have. Measured rather than
    // assumed -- `HEX(UPPER(_utf8mb4 0xEA9FB6))` is `EA9FB6` on 8.3 under
    // both utf8mb4 collations, so the pair is absent from MySQL's tables and
    // not from this transcription of them.
    ('\u{A7B5}', '\u{A7B7}', 2, '\u{A7B4}'),
    ('\u{AB53}', '\u{AB53}', 1, '\u{A7B3}'),
    ('\u{AB70}', '\u{ABBF}', 1, '\u{13A0}'),    // Cherokee
    ('\u{FF41}', '\u{FF5A}', 1, '\u{FF21}'),    // Fullwidth
    ('\u{10428}', '\u{1044F}', 1, '\u{10400}'), // Deseret
    ('\u{104D8}', '\u{104FB}', 1, '\u{104B0}'), // Osage
    ('\u{10CC0}', '\u{10CF2}', 1, '\u{10C80}'), // Old
    ('\u{118C0}', '\u{118DF}', 1, '\u{118A0}'), // Warang
    ('\u{1E922}', '\u{1E943}', 1, '\u{1E900}'), // Adlam
];

/// `LOWER` under `utf8mb4_0900_ai_ci`, in the same shape.
///
/// A separate table, not the inverse of `UPPER`. 1321 characters change under
/// one and 1304 under the other, and composing them is the identity in
/// neither direction: measured, `UPPER(LOWER(x))` differs from `UPPER(x)` for
/// 23 characters — `İ`, `ẞ` and the Kelvin, Ohm and Angstrom signs among them
/// — and `LOWER(UPPER(x))` from `LOWER(x)` for 25, including `µ`, `ı`, `ſ` and
/// `ς`.
const LOWER: &[(char, char, u32, char)] = &[
    ('\u{0041}', '\u{005A}', 1, '\u{0061}'), // Latin
    ('\u{00C0}', '\u{00D6}', 1, '\u{00E0}'),
    ('\u{00D8}', '\u{00DE}', 1, '\u{00F8}'),
    ('\u{0100}', '\u{012E}', 2, '\u{0101}'),
    ('\u{0130}', '\u{0130}', 1, '\u{0069}'),
    ('\u{0132}', '\u{0136}', 2, '\u{0133}'),
    ('\u{0139}', '\u{0147}', 2, '\u{013A}'),
    ('\u{014A}', '\u{0176}', 2, '\u{014B}'),
    ('\u{0178}', '\u{0178}', 1, '\u{00FF}'),
    ('\u{0179}', '\u{017D}', 2, '\u{017A}'),
    ('\u{0181}', '\u{0181}', 1, '\u{0253}'),
    ('\u{0182}', '\u{0184}', 2, '\u{0183}'),
    ('\u{0186}', '\u{0186}', 1, '\u{0254}'),
    ('\u{0187}', '\u{0187}', 1, '\u{0188}'),
    ('\u{0189}', '\u{018A}', 1, '\u{0256}'),
    ('\u{018B}', '\u{018B}', 1, '\u{018C}'),
    ('\u{018E}', '\u{018E}', 1, '\u{01DD}'),
    ('\u{018F}', '\u{018F}', 1, '\u{0259}'),
    ('\u{0190}', '\u{0190}', 1, '\u{025B}'),
    ('\u{0191}', '\u{0191}', 1, '\u{0192}'),
    ('\u{0193}', '\u{0193}', 1, '\u{0260}'),
    ('\u{0194}', '\u{0194}', 1, '\u{0263}'),
    ('\u{0196}', '\u{0196}', 1, '\u{0269}'),
    ('\u{0197}', '\u{0197}', 1, '\u{0268}'),
    ('\u{0198}', '\u{0198}', 1, '\u{0199}'),
    ('\u{019C}', '\u{019C}', 1, '\u{026F}'),
    ('\u{019D}', '\u{019D}', 1, '\u{0272}'),
    ('\u{019F}', '\u{019F}', 1, '\u{0275}'),
    ('\u{01A0}', '\u{01A4}', 2, '\u{01A1}'),
    ('\u{01A6}', '\u{01A6}', 1, '\u{0280}'),
    ('\u{01A7}', '\u{01A7}', 1, '\u{01A8}'),
    ('\u{01A9}', '\u{01A9}', 1, '\u{0283}'),
    ('\u{01AC}', '\u{01AC}', 1, '\u{01AD}'),
    ('\u{01AE}', '\u{01AE}', 1, '\u{0288}'),
    ('\u{01AF}', '\u{01AF}', 1, '\u{01B0}'),
    ('\u{01B1}', '\u{01B2}', 1, '\u{028A}'),
    ('\u{01B3}', '\u{01B5}', 2, '\u{01B4}'),
    ('\u{01B7}', '\u{01B7}', 1, '\u{0292}'),
    ('\u{01B8}', '\u{01B8}', 1, '\u{01B9}'),
    ('\u{01BC}', '\u{01BC}', 1, '\u{01BD}'),
    ('\u{01C4}', '\u{01C4}', 1, '\u{01C6}'),
    ('\u{01C5}', '\u{01C5}', 1, '\u{01C6}'),
    ('\u{01C7}', '\u{01C7}', 1, '\u{01C9}'),
    ('\u{01C8}', '\u{01C8}', 1, '\u{01C9}'),
    ('\u{01CA}', '\u{01CA}', 1, '\u{01CC}'),
    ('\u{01CB}', '\u{01DB}', 2, '\u{01CC}'),
    ('\u{01DE}', '\u{01EE}', 2, '\u{01DF}'),
    ('\u{01F1}', '\u{01F1}', 1, '\u{01F3}'),
    ('\u{01F2}', '\u{01F4}', 2, '\u{01F3}'),
    ('\u{01F6}', '\u{01F6}', 1, '\u{0195}'),
    ('\u{01F7}', '\u{01F7}', 1, '\u{01BF}'),
    ('\u{01F8}', '\u{021E}', 2, '\u{01F9}'),
    ('\u{0220}', '\u{0220}', 1, '\u{019E}'),
    ('\u{0222}', '\u{0232}', 2, '\u{0223}'),
    ('\u{023A}', '\u{023A}', 1, '\u{2C65}'),
    ('\u{023B}', '\u{023B}', 1, '\u{023C}'),
    ('\u{023D}', '\u{023D}', 1, '\u{019A}'),
    ('\u{023E}', '\u{023E}', 1, '\u{2C66}'),
    ('\u{0241}', '\u{0241}', 1, '\u{0242}'),
    ('\u{0243}', '\u{0243}', 1, '\u{0180}'),
    ('\u{0244}', '\u{0244}', 1, '\u{0289}'),
    ('\u{0245}', '\u{0245}', 1, '\u{028C}'),
    ('\u{0246}', '\u{024E}', 2, '\u{0247}'),
    ('\u{0370}', '\u{0372}', 2, '\u{0371}'), // Greek
    ('\u{0376}', '\u{0376}', 1, '\u{0377}'),
    ('\u{037F}', '\u{037F}', 1, '\u{03F3}'),
    ('\u{0386}', '\u{0386}', 1, '\u{03AC}'),
    ('\u{0388}', '\u{038A}', 1, '\u{03AD}'),
    ('\u{038C}', '\u{038C}', 1, '\u{03CC}'),
    ('\u{038E}', '\u{038F}', 1, '\u{03CD}'),
    ('\u{0391}', '\u{03A1}', 1, '\u{03B1}'),
    ('\u{03A3}', '\u{03AB}', 1, '\u{03C3}'),
    ('\u{03CF}', '\u{03CF}', 1, '\u{03D7}'),
    ('\u{03D8}', '\u{03EE}', 2, '\u{03D9}'),
    ('\u{03F4}', '\u{03F4}', 1, '\u{03B8}'),
    ('\u{03F7}', '\u{03F7}', 1, '\u{03F8}'),
    ('\u{03F9}', '\u{03F9}', 1, '\u{03F2}'),
    ('\u{03FA}', '\u{03FA}', 1, '\u{03FB}'),
    ('\u{03FD}', '\u{03FF}', 1, '\u{037B}'),
    ('\u{0400}', '\u{040F}', 1, '\u{0450}'), // Cyrillic
    ('\u{0410}', '\u{042F}', 1, '\u{0430}'),
    ('\u{0460}', '\u{0480}', 2, '\u{0461}'),
    ('\u{048A}', '\u{04BE}', 2, '\u{048B}'),
    ('\u{04C0}', '\u{04C0}', 1, '\u{04CF}'),
    ('\u{04C1}', '\u{04CD}', 2, '\u{04C2}'),
    ('\u{04D0}', '\u{052E}', 2, '\u{04D1}'),
    ('\u{0531}', '\u{0556}', 1, '\u{0561}'), // Armenian
    ('\u{10A0}', '\u{10C5}', 1, '\u{2D00}'), // Georgian
    ('\u{10C7}', '\u{10C7}', 1, '\u{2D27}'),
    ('\u{10CD}', '\u{10CD}', 1, '\u{2D2D}'),
    ('\u{13A0}', '\u{13EF}', 1, '\u{AB70}'), // Cherokee
    ('\u{13F0}', '\u{13F5}', 1, '\u{13F8}'),
    ('\u{1E00}', '\u{1E94}', 2, '\u{1E01}'), // Latin
    ('\u{1E9E}', '\u{1E9E}', 1, '\u{00DF}'),
    ('\u{1EA0}', '\u{1EFE}', 2, '\u{1EA1}'),
    ('\u{1F08}', '\u{1F0F}', 1, '\u{1F00}'), // Greek
    ('\u{1F18}', '\u{1F1D}', 1, '\u{1F10}'),
    ('\u{1F28}', '\u{1F2F}', 1, '\u{1F20}'),
    ('\u{1F38}', '\u{1F3F}', 1, '\u{1F30}'),
    ('\u{1F48}', '\u{1F4D}', 1, '\u{1F40}'),
    ('\u{1F59}', '\u{1F5F}', 2, '\u{1F51}'),
    ('\u{1F68}', '\u{1F6F}', 1, '\u{1F60}'),
    ('\u{1F88}', '\u{1F8F}', 1, '\u{1F80}'),
    ('\u{1F98}', '\u{1F9F}', 1, '\u{1F90}'),
    ('\u{1FA8}', '\u{1FAF}', 1, '\u{1FA0}'),
    ('\u{1FB8}', '\u{1FB9}', 1, '\u{1FB0}'),
    ('\u{1FBA}', '\u{1FBB}', 1, '\u{1F70}'),
    ('\u{1FBC}', '\u{1FBC}', 1, '\u{1FB3}'),
    ('\u{1FC8}', '\u{1FCB}', 1, '\u{1F72}'),
    ('\u{1FCC}', '\u{1FCC}', 1, '\u{1FC3}'),
    ('\u{1FD8}', '\u{1FD9}', 1, '\u{1FD0}'),
    ('\u{1FDA}', '\u{1FDB}', 1, '\u{1F76}'),
    ('\u{1FE8}', '\u{1FE9}', 1, '\u{1FE0}'),
    ('\u{1FEA}', '\u{1FEB}', 1, '\u{1F7A}'),
    ('\u{1FEC}', '\u{1FEC}', 1, '\u{1FE5}'),
    ('\u{1FF8}', '\u{1FF9}', 1, '\u{1F78}'),
    ('\u{1FFA}', '\u{1FFB}', 1, '\u{1F7C}'),
    ('\u{1FFC}', '\u{1FFC}', 1, '\u{1FF3}'),
    ('\u{2126}', '\u{2126}', 1, '\u{03C9}'), // Ohm
    ('\u{212A}', '\u{212A}', 1, '\u{006B}'), // Kelvin
    ('\u{212B}', '\u{212B}', 1, '\u{00E5}'), // Angstrom
    ('\u{2132}', '\u{2132}', 1, '\u{214E}'), // Turned
    ('\u{2160}', '\u{216F}', 1, '\u{2170}'), // Roman
    ('\u{2183}', '\u{2183}', 1, '\u{2184}'),
    ('\u{24B6}', '\u{24CF}', 1, '\u{24D0}'), // Circled
    ('\u{2C00}', '\u{2C2E}', 1, '\u{2C30}'), // Glagolitic
    ('\u{2C60}', '\u{2C60}', 1, '\u{2C61}'), // Latin
    ('\u{2C62}', '\u{2C62}', 1, '\u{026B}'),
    ('\u{2C63}', '\u{2C63}', 1, '\u{1D7D}'),
    ('\u{2C64}', '\u{2C64}', 1, '\u{027D}'),
    ('\u{2C67}', '\u{2C6B}', 2, '\u{2C68}'),
    ('\u{2C6D}', '\u{2C6D}', 1, '\u{0251}'),
    ('\u{2C6E}', '\u{2C6E}', 1, '\u{0271}'),
    ('\u{2C6F}', '\u{2C6F}', 1, '\u{0250}'),
    ('\u{2C70}', '\u{2C70}', 1, '\u{0252}'),
    ('\u{2C72}', '\u{2C72}', 1, '\u{2C73}'),
    ('\u{2C75}', '\u{2C75}', 1, '\u{2C76}'),
    ('\u{2C7E}', '\u{2C7F}', 1, '\u{023F}'),
    ('\u{2C80}', '\u{2CE2}', 2, '\u{2C81}'), // Coptic
    ('\u{2CEB}', '\u{2CED}', 2, '\u{2CEC}'),
    ('\u{2CF2}', '\u{2CF2}', 1, '\u{2CF3}'),
    ('\u{A640}', '\u{A66C}', 2, '\u{A641}'), // Cyrillic
    ('\u{A680}', '\u{A69A}', 2, '\u{A681}'),
    ('\u{A722}', '\u{A72E}', 2, '\u{A723}'), // Latin
    ('\u{A732}', '\u{A76E}', 2, '\u{A733}'),
    ('\u{A779}', '\u{A77B}', 2, '\u{A77A}'),
    ('\u{A77D}', '\u{A77D}', 1, '\u{1D79}'),
    ('\u{A77E}', '\u{A786}', 2, '\u{A77F}'),
    ('\u{A78B}', '\u{A78B}', 1, '\u{A78C}'),
    ('\u{A78D}', '\u{A78D}', 1, '\u{0265}'),
    ('\u{A790}', '\u{A792}', 2, '\u{A791}'),
    ('\u{A796}', '\u{A7A8}', 2, '\u{A797}'),
    ('\u{A7AA}', '\u{A7AA}', 1, '\u{0266}'),
    ('\u{A7AB}', '\u{A7AB}', 1, '\u{025C}'),
    ('\u{A7AC}', '\u{A7AC}', 1, '\u{0261}'),
    ('\u{A7AD}', '\u{A7AD}', 1, '\u{026C}'),
    ('\u{A7AE}', '\u{A7AE}', 1, '\u{026A}'),
    ('\u{A7B0}', '\u{A7B0}', 1, '\u{029E}'),
    ('\u{A7B1}', '\u{A7B1}', 1, '\u{0287}'),
    ('\u{A7B2}', '\u{A7B2}', 1, '\u{029D}'),
    ('\u{A7B3}', '\u{A7B3}', 1, '\u{AB53}'),
    // `U+A7F5` -> `U+A7F6` is absent here for the same measured reason as its
    // mirror in `UPPER`.
    ('\u{A7B4}', '\u{A7B6}', 2, '\u{A7B5}'),
    ('\u{FF21}', '\u{FF3A}', 1, '\u{FF41}'),    // Fullwidth
    ('\u{10400}', '\u{10427}', 1, '\u{10428}'), // Deseret
    ('\u{104B0}', '\u{104D3}', 1, '\u{104D8}'), // Osage
    ('\u{10C80}', '\u{10CB2}', 1, '\u{10CC0}'), // Old
    ('\u{118A0}', '\u{118BF}', 1, '\u{118C0}'), // Warang
    ('\u{1E900}', '\u{1E921}', 1, '\u{1E922}'), // Adlam
];

/// MySQL's `UPPER` over a character string.
pub fn upper(s: &str) -> String {
    fold(s, UPPER)
}

/// MySQL's `LOWER` over a character string.
pub fn lower(s: &str) -> String {
    fold(s, LOWER)
}

/// The one character `table` gives for `c`, or `c` where it has no entry.
fn mapped(table: &[(char, char, u32, char)], c: char) -> char {
    // The last run that starts at or below `c`. Sorted and non-overlapping is
    // what makes that the only run that could hold it.
    let index = table.partition_point(|&(first, ..)| first <= c);
    let Some(&(first, last, step, to)) = index.checked_sub(1).map(|i| &table[i]) else {
        return c;
    };
    let offset = c as u32 - first as u32;
    // Past the run's end, or between two of its members where the step skips
    // one — a capital in a run of small letters.
    if c > last || offset % step != 0 {
        return c;
    }
    // Unreachable: `every_entry_maps_to_a_character_and_nineteen_of_them_grow`
    // walks all 2625 mappings.
    char::from_u32(to as u32 + offset).unwrap_or(c)
}

/// One folded character per character, except that a fold wider in UTF-8 than
/// the character it replaces is declined and the character kept.
///
/// The exception is what keeps this from being `chars().map(...)`, and it is
/// nineteen characters wide. 8.3 folds into a buffer the size of the argument
/// and truncates when one of them overflows it; this returns the unfolded
/// character instead, which is `utf8mb4_general_ci`'s answer and so 8.3's own
/// answer for a literal. The module doc has the measurement the choice was
/// made on, and #0154 has what it costs over a column.
///
/// One consequence worth stating where it is relied on: the answer has exactly
/// as many characters as the argument, and never more bytes.
fn fold(s: &str, table: &[(char, char, u32, char)]) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let folded = mapped(table, c);
        out.push(if folded.len_utf8() > c.len_utf8() {
            c
        } else {
            folded
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything the tables say that Unicode would say differently. Every
    /// expectation is 8.3's answer under `utf8mb4_0900_ai_ci`.
    #[test]
    fn matches_mysql_where_unicode_disagrees() {
        // A sequence in Unicode's full mapping, itself here.
        assert_eq!(upper("stra\u{df}e"), "STRA\u{df}E");
        assert_eq!(upper("\u{fb01}"), "\u{fb01}"); // ﬁ, not FI
        assert_eq!(upper("\u{1f0}"), "\u{1f0}"); // ǰ, not J + U+030C
        assert_eq!(upper("\u{149}"), "\u{149}"); // ŉ, not ʼN
        assert_eq!(upper("\u{1e96}"), "\u{1e96}"); // ẖ, not H + U+0331
        assert_eq!(upper("\u{390}"), "\u{390}"); // ΐ, not three characters
        assert_eq!(lower("\u{130}"), "i"); // İ, not i + U+0307

        // A sequence in Unicode and one character here, so "leave a sequence
        // alone" is not the rule either.
        assert_eq!(upper("\u{1f80}"), "\u{1f88}");
        assert_eq!(upper("\u{1fb3}"), "\u{1fbc}");
        // Case pairs 8.3's tables stop short of: they are Unicode 9.0, and
        // Georgian's Mtavruli capitals are Unicode 11.
        assert_eq!(upper("\u{10d0}"), "\u{10d0}"); // Georgian ა stays put
        assert_eq!(lower("\u{1c90}"), "\u{1c90}"); // and so does Ა
        assert_eq!(upper("\u{a7b9}"), "\u{a7b9}");
        // The ordinary case, which the table must not have broken.
        assert_eq!(upper("stra\u{df}e ab\u{e9}"), "STRA\u{df}E AB\u{c9}");
        assert_eq!(lower("STRA\u{df}E AB\u{c9}"), "stra\u{df}e ab\u{e9}");
        assert_eq!(upper(""), "");
    }

    /// Above the BMP, where `utf8mb4_0900_ai_ci` still folds and its four
    /// bytes stay four.
    #[test]
    fn a_supplementary_character_folds() {
        assert_eq!(upper("\u{10428}"), "\u{10400}"); // Deseret
        assert_eq!(lower("\u{10400}"), "\u{10428}");
        assert_eq!(upper("\u{1e922}"), "\u{1e900}"); // Adlam
        assert_eq!(upper("\u{1f600}"), "\u{1f600}"); // and one with no case
    }

    /// Neither direction may be derived from the other.
    #[test]
    fn upper_and_lower_are_separate_tables() {
        assert_eq!(lower("\u{b5}"), "\u{b5}");
        assert_eq!(upper("\u{b5}"), "\u{39c}");
        assert_eq!(lower(&upper("\u{b5}")), "\u{3bc}");
        assert_eq!(upper("\u{130}"), "\u{130}");
        assert_eq!(upper(&lower("\u{130}")), "I");
        assert_eq!(lower("\u{212a}"), "k"); // the Kelvin sign
        assert_eq!(upper("\u{212a}"), "\u{212a}");
        assert_eq!(lower("\u{17f}"), "\u{17f}");
        assert_eq!(lower(&upper("\u{17f}")), "s");
        // `ß` uppercases to itself and `ẞ` lowercases to `ß`, so the pair is
        // one-way.
        assert_eq!(upper("\u{df}"), "\u{df}");
        assert_eq!(lower("\u{1e9e}"), "\u{df}");
        assert_eq!(upper("\u{1e9e}"), "\u{1e9e}");
    }

    /// A run with a step of 2 answers for its members and not for what falls
    /// between them, which is the one thing the compressed shape can get wrong
    /// without any of the cases above noticing.
    #[test]
    fn a_stepped_run_skips_what_lies_between_its_members() {
        // U+0100..U+012E step 2 downward, U+0101..U+012F step 2 upward.
        assert_eq!(lower("\u{100}"), "\u{101}");
        assert_eq!(lower("\u{102}"), "\u{103}");
        assert_eq!(upper("\u{101}"), "\u{100}");
        assert_eq!(upper("\u{103}"), "\u{102}");
        // The other member of each pair is already folded and must not move.
        assert_eq!(lower("\u{101}"), "\u{101}");
        assert_eq!(upper("\u{100}"), "\u{100}");
    }

    /// The nineteen characters whose fold is wider in UTF-8 than they are.
    ///
    /// 8.3 folds into a buffer the size of the argument and truncates when one
    /// of these overflows it — `UPPER('ȿ')` is the empty string over a column
    /// there. This engine declines the fold instead, which is
    /// `utf8mb4_general_ci`'s answer and so 8.3's own for a literal. Every
    /// expectation *for one of the nineteen* below is that literal answer,
    /// read off 8.3; the module doc carries the column answers and the
    /// measurement the choice rests on.
    #[test]
    fn a_fold_that_would_grow_is_declined_rather_than_truncated() {
        assert_eq!(upper("\u{23f}"), "\u{23f}"); // two bytes in, three needed
        assert_eq!(upper("a\u{23f}b"), "A\u{23f}B"); // and the `b` survives
        assert_eq!(upper("\u{23f}aa"), "\u{23f}AA");
        assert_eq!(upper("\u{23f}\u{23f}a"), "\u{23f}\u{23f}A");
        assert_eq!(upper("aa\u{23f}a"), "AA\u{23f}A");
        assert_eq!(lower("\u{23a}"), "\u{23a}"); // Ⱥ, the same thing downward
        assert_eq!(lower("\u{23e}"), "\u{23e}");
        assert_eq!(lower("\u{130}\u{23a}"), "i\u{23a}");

        // The character each of them would have folded *into* is not itself
        // affected: this is a rule about the fold's width, not a hole in the
        // table. `Ȿ` lowercases back to `ȿ` because that fold is three bytes
        // to two, so it fits.
        //
        // Not a literal answer, and the one line here that is not: 8.3 answers
        // `E2B1BE` for `LOWER('Ȿ')` under `general_ci` and `C8BF` under
        // `0900_ai_ci`, both measured. `Ȿ` is one of the 616 characters the
        // two collations simply disagree about, not one of the nineteen.
        assert_eq!(upper("\u{2c7e}"), "\u{2c7e}");
        assert_eq!(lower("\u{2c7e}"), "\u{23f}");

        // Nothing is ever lost and nothing ever grows, which is the property
        // the choice was made for.
        for text in [
            "a\u{23f}b",
            "\u{23a}\u{23f}\u{130}",
            "stra\u{df}e",
            "\u{10428}\u{1f600}",
        ] {
            for folded in [upper(text), lower(text)] {
                assert_eq!(folded.chars().count(), text.chars().count(), "{text}");
                assert!(folded.len() <= text.len(), "{text}");
            }
        }
        let long: String = std::iter::repeat_n('\u{e9}', 500).collect();
        assert_eq!(upper(&long).chars().count(), 500);
    }

    /// `mapped` binary-searches, so an unsorted or overlapping table is a
    /// silent wrong answer rather than a failure.
    #[test]
    fn the_tables_are_sorted_and_disjoint() {
        for table in [UPPER, LOWER] {
            for pair in table.windows(2) {
                let (first, last, step, _) = pair[0];
                assert!(first <= last, "{:?} is inverted", pair[0]);
                assert!(step == 1 || step == 2, "{:?} has an odd step", pair[0]);
                assert!(last < pair[1].0, "{pair:?} is out of order or overlaps");
            }
            let &(first, last, step, _) = table.last().expect("the table is not empty");
            assert!(first <= last && (step == 1 || step == 2));
        }
    }

    /// Every mapping the tables describe is a real character, reads back out
    /// of its own run, and is one of exactly nineteen that grow in UTF-8.
    ///
    /// The count is the guard on `fold`: the truncation only matters because
    /// some folds are wider than their source, and a table that grew that set
    /// silently would change answers `fold` is not tested on.
    #[test]
    fn every_entry_maps_to_a_character_and_nineteen_of_them_grow() {
        let (mut mappings, mut grew) = (0, 0);
        for table in [UPPER, LOWER] {
            for &(first, last, step, to) in table {
                let mut point = first as u32;
                while point <= last as u32 {
                    let from = char::from_u32(point).expect("a run member is a character");
                    let into = char::from_u32(to as u32 + (point - first as u32))
                        .unwrap_or_else(|| panic!("U+{point:04X} maps to no character"));
                    assert_eq!(
                        mapped(table, from),
                        into,
                        "U+{point:04X} does not read back out of its own run"
                    );
                    if into.len_utf8() > from.len_utf8() {
                        grew += 1;
                    }
                    mappings += 1;
                    point += step;
                }
            }
        }
        // 1321 upward and 1304 downward, counted off 8.3.
        assert_eq!(mappings, 2625);
        assert_eq!(grew, 19);
    }
}
