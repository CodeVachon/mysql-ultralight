//! Which collations this engine has, what each of them *does*, and reading the
//! name of one.
//!
//! One place, because a collation reaches a client by two routes that have to
//! agree: `information_schema.COLLATIONS` says which ones exist, and a
//! `COLLATE` clause is checked against the same list. They were separate — the clause
//! was checked only for a `_ci` suffix — so `COLLATE bogus_ci` was accepted,
//! lowercased, written into the append-only state image and reported back to
//! whatever introspected the schema, while `COLLATIONS` went on listing four
//! names that did not include it. 8.3 answers 1273 there.
//!
//! Reading the name is here for the same reason. The column-level clause, the
//! table-level clause and the `x COLLATE c` expression are three spellings of
//! one thing, and each having its own reader is how two of them came to
//! disagree about `utf8mb4_bin`.
//!
//! And *doing* the comparison is here because for a long time nothing did.
//! Every clause above was recorded and then ignored: the engine folded case,
//! compared codepoints, and gave the same answer whatever the column said. It
//! was one comparison wearing four names, which is three wrong answers. See
//! [`Collation`].

use std::cmp::Ordering;

use sqlparser::ast::{Expr, ObjectName};

use crate::error::{unsupported, EngineError, EngineResult};
use crate::name;

/// What comparing two strings *means*, which the column decides.
///
/// Not a label. A collation decides which rows `=` returns, which rows `LIKE`
/// matches, what `ORDER BY` puts first, which values `GROUP BY` and `DISTINCT`
/// fold together and which pair a UNIQUE key refuses. All of those were one
/// answer here — fold ASCII case, then compare codepoints — while the DDL that
/// said otherwise was stored and never read. Measured against 8.3 over rows
/// `café`, `Cafe`, `Straße` in a column declared `utf8mb4_unicode_ci`, the way
/// the target application declares all 1537 of its text columns:
///
/// ```text
///     WHERE name = 'cafe'      mysql café + Cafe        engine Cafe only
///     WHERE name LIKE 'caf%'   mysql café + Cafe        engine café only
///     WHERE name = 'strasse'   mysql Straße             engine no rows
///     ORDER BY name            mysql café,Cafe,Straße   engine Cafe,café,Straße
/// ```
///
/// Wrong result sets, so [`Collation::compare`] is what `=` asks rather than
/// one shared rule. See #0129, #0134 and #0152, which are this one gap seen
/// from three sides.
///
/// The four here are the four the engine can honour; [`KNOWN`] is the same list
/// as data, and a `COLLATE` clause naming anything else is refused rather than
/// silently treated as one of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Collation {
    /// `utf8mb4_0900_ai_ci`, MySQL 8's server default and this engine's.
    ///
    /// Case-insensitive, accent-insensitive, and NO PAD: `'a ' = 'a'` is 0
    /// here where it is 1 under every other collation below. Measured — the
    /// `_0900_` collations are the only ones in 8.3 that do not pad.
    #[default]
    Ai0900Ci,
    /// `utf8mb4_unicode_ci`, which the target application declares everywhere.
    ///
    /// Case- and accent-insensitive like the default, PAD SPACE unlike it, and
    /// built on an older UCA table that has fewer expansions: `æ` is its own
    /// letter here where the default folds it to `ae`. Measured, not inferred.
    UnicodeCi,
    /// `utf8mb4_general_ci`, the MySQL 5.x default.
    ///
    /// Case-insensitive and *nearly* accent-insensitive: it folds the
    /// precomposed accented letters to their base but has no expansions at
    /// all, so `ß` is `s` rather than `ss` and a combining mark is a character
    /// of its own. PAD SPACE.
    GeneralCi,
    /// `utf8mb4_bin`, which compares the characters exactly.
    ///
    /// Case- and accent-*sensitive*, so it is what keeps a test of this
    /// mechanism honest: a fold applied unconditionally would pass every
    /// case-insensitive assertion and this one would still fail. PAD SPACE.
    Bin,
}

impl Collation {
    /// The name 8.3 records for this collation, which is what a clause is
    /// written as and what `information_schema` reports.
    pub fn name(self) -> &'static str {
        match self {
            Collation::Ai0900Ci => "utf8mb4_0900_ai_ci",
            Collation::UnicodeCi => "utf8mb4_unicode_ci",
            Collation::GeneralCi => "utf8mb4_general_ci",
            Collation::Bin => "utf8mb4_bin",
        }
    }

    /// The collation a name refers to, where this engine has it.
    ///
    /// The name is expected already lowercased by [`named`], which is the only
    /// thing that produces one.
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Collation::Ai0900Ci,
            Collation::UnicodeCi,
            Collation::GeneralCi,
            Collation::Bin,
        ]
        .into_iter()
        .find(|c| c.name() == name)
    }

    /// Whether trailing spaces are ignored when comparing.
    ///
    /// PAD SPACE for everything but the `_0900_` collations, which is measured
    /// rather than remembered: `'a ' = 'a'` under a column of each is 0 for
    /// `utf8mb4_0900_ai_ci` and 1 for `utf8mb4_unicode_ci`,
    /// `utf8mb4_general_ci` and `utf8mb4_bin`. A literal pair `'a ' = 'a'` is
    /// 0, because both literals carry the connection's `_0900_` collation.
    ///
    /// Only the space pads. `'a' = 'a\t'` is 0 under all four.
    pub fn pads(self) -> bool {
        !matches!(self, Collation::Ai0900Ci)
    }

    /// Whether a combining mark is ignored, which is the `_ai_` in the name.
    ///
    /// True for the two UCA collations and false for `utf8mb4_general_ci`,
    /// which folds only the *precomposed* accented letters. Measured: `'a' ||
    /// U+0301 || 'b' = 'ab'` is 1 under `utf8mb4_0900_ai_ci` and
    /// `utf8mb4_unicode_ci`, 0 under `utf8mb4_general_ci` and `utf8mb4_bin`.
    /// It matters because a client can hand over decomposed text without
    /// meaning to — macOS filenames are NFD.
    fn ignores_marks(self) -> bool {
        matches!(self, Collation::Ai0900Ci | Collation::UnicodeCi)
    }

    /// The characters this collation folds differently from the other three.
    fn overrides(self) -> &'static [(char, &'static str)] {
        match self {
            Collation::Ai0900Ci => FOLD_AI_CI,
            Collation::UnicodeCi => FOLD_UNICODE_CI,
            Collation::GeneralCi => FOLD_GENERAL_CI,
            Collation::Bin => &[],
        }
    }

    /// What one character compares as.
    ///
    /// Zero, one or several characters: a combining mark folds away to nothing
    /// under an accent-insensitive collation, and `ß` folds to *two*
    /// characters under the two that have expansions. That is why this yields
    /// an iterator rather than a `char` — a fold that could only be one
    /// character for one could not answer `'ss' = 'ß'`.
    fn fold_char(self, c: char) -> Folded {
        if self == Collation::Bin {
            return Folded::Same(Some(c));
        }
        if self.ignores_marks() && COMBINING_MARKS.contains(&c) {
            return Folded::Same(None);
        }
        if let Some(folded) = lookup(self.overrides(), c).or_else(|| lookup(FOLD, c)) {
            return Folded::Table(folded.chars());
        }
        Folded::Lower(c.to_lowercase())
    }

    /// The characters `text` compares as, in order.
    ///
    /// The single source for every question this collation answers: `=`,
    /// `ORDER BY`, `GROUP BY`, `DISTINCT` and a unique key all read this, so
    /// none of them can decide something the others do not. They did disagree
    /// before — `compare` folded case with an iterator and `collation_key`
    /// with `to_lowercase` — and two spellings of one rule is how a `GROUP BY`
    /// comes to split a pair that `=` calls equal.
    fn folded(self, text: &str) -> impl Iterator<Item = char> + '_ {
        let text = if self.pads() {
            text.trim_end_matches(' ')
        } else {
            text
        };
        text.chars().flat_map(move |c| self.fold_char(c))
    }

    /// Compares two strings under this collation.
    pub fn compare(self, a: &str, b: &str) -> Ordering {
        self.folded(a).cmp(self.folded(b))
    }

    /// Whether two strings are the same under this collation.
    pub fn same(self, a: &str, b: &str) -> bool {
        self.compare(a, b) == Ordering::Equal
    }

    /// The key this collation sorts and de-duplicates a string under.
    ///
    /// `compare` written down rather than a second rule: two strings have the
    /// same key exactly when `compare` calls them equal, because both read
    /// [`Collation::folded`].
    pub fn key(self, text: &str) -> String {
        self.folded(text).collect()
    }

    /// Whether one character of a `LIKE` pattern matches one of the subject.
    ///
    /// Character by character, and deliberately not through
    /// [`Collation::compare`]: MySQL's `LIKE` does not apply the collation's
    /// *expansions*, so `'Straße' LIKE 'strasse'` is 0 on 8.3 under all four
    /// collations while `'Straße' = 'strasse'` is 1 under two of them.
    /// Measured, along with the two facts that pin the rule down: `'Straße'
    /// LIKE 'stra_e'` is 1 — `ß` is one character to `_` — and `'Straße' LIKE
    /// 'straße'` is 1 under every `_ci` collation and 0 under `utf8mb4_bin`.
    ///
    /// Padding is not applied either: `'a  ' LIKE 'a'` is 0 under all four,
    /// including the three that pad for `=`.
    pub fn chars_match(self, subject: char, pattern: char) -> bool {
        self.fold_char(subject).eq(self.fold_char(pattern))
    }
}

/// What one character folds to, which is not always one character.
enum Folded {
    /// A measured fold, which may be an expansion such as `ß` to `ss`.
    Table(std::str::Chars<'static>),
    /// No entry, so the collation's own case fold is the whole of it.
    Lower(std::char::ToLowercase),
    /// The character itself, or nothing where it is ignorable.
    Same(Option<char>),
}

impl Iterator for Folded {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        match self {
            Folded::Table(chars) => chars.next(),
            Folded::Lower(lower) => lower.next(),
            Folded::Same(c) => c.take(),
        }
    }
}

/// The block NFD produces for Latin text, which the `_ai_` collations ignore.
///
/// The block rather than the whole `Mn` general category, which would need a
/// Unicode table this engine does not carry. Every accent a Latin-script
/// client decomposes to is in here.
const COMBINING_MARKS: std::ops::RangeInclusive<char> = '\u{300}'..='\u{36f}';

/// Reads one entry out of a table sorted by character.
///
/// `binary_search_by_key` because these run per character per comparison, and
/// a linear scan of 162 entries is the sort of cost that turns a test suite's
/// `ORDER BY` into a visible pause.
fn lookup(table: &'static [(char, &'static str)], c: char) -> Option<&'static str> {
    table
        .binary_search_by_key(&c, |(k, _)| *k)
        .ok()
        .map(|i| table[i].1)
}

/// What every collation this engine has folds the same way.
///
/// Measured on `mysql:8.3` rather than written from memory: for every
/// character in Latin-1 Supplement and Latin Extended-A, the shortest ASCII
/// string the server calls equal to it under each of the three `_ci`
/// collations. Where all three agreed and the answer was not simply the
/// character's own lower case, the entry is here; where they disagreed it is
/// in one of the three tables below. 214 of the 236 characters agreed.
///
/// Sorted by character, which [`lookup`] binary-searches and a test checks —
/// an entry added out of order would be silently invisible rather than fail to
/// build.
///
/// Outside this range a character folds to its own lower case, which is exact
/// for equality between two characters MySQL also separates and wrong for a
/// pair it folds together. Latin Extended-B onwards, Greek and Cyrillic are
/// the gap; the target application's text is Latin-1 and Latin Extended-A.
const FOLD: &[(char, &str)] = &[
    ('\u{c0}', "a"),
    ('\u{c1}', "a"),
    ('\u{c2}', "a"),
    ('\u{c3}', "a"),
    ('\u{c4}', "a"),
    ('\u{c5}', "a"),
    ('\u{c7}', "c"),
    ('\u{c8}', "e"),
    ('\u{c9}', "e"),
    ('\u{ca}', "e"),
    ('\u{cb}', "e"),
    ('\u{cc}', "i"),
    ('\u{cd}', "i"),
    ('\u{ce}', "i"),
    ('\u{cf}', "i"),
    ('\u{d1}', "n"),
    ('\u{d2}', "o"),
    ('\u{d3}', "o"),
    ('\u{d4}', "o"),
    ('\u{d5}', "o"),
    ('\u{d6}', "o"),
    ('\u{d9}', "u"),
    ('\u{da}', "u"),
    ('\u{db}', "u"),
    ('\u{dc}', "u"),
    ('\u{dd}', "y"),
    ('\u{e0}', "a"),
    ('\u{e1}', "a"),
    ('\u{e2}', "a"),
    ('\u{e3}', "a"),
    ('\u{e4}', "a"),
    ('\u{e5}', "a"),
    ('\u{e7}', "c"),
    ('\u{e8}', "e"),
    ('\u{e9}', "e"),
    ('\u{ea}', "e"),
    ('\u{eb}', "e"),
    ('\u{ec}', "i"),
    ('\u{ed}', "i"),
    ('\u{ee}', "i"),
    ('\u{ef}', "i"),
    ('\u{f1}', "n"),
    ('\u{f2}', "o"),
    ('\u{f3}', "o"),
    ('\u{f4}', "o"),
    ('\u{f5}', "o"),
    ('\u{f6}', "o"),
    ('\u{f9}', "u"),
    ('\u{fa}', "u"),
    ('\u{fb}', "u"),
    ('\u{fc}', "u"),
    ('\u{fd}', "y"),
    ('\u{ff}', "y"),
    ('\u{100}', "a"),
    ('\u{101}', "a"),
    ('\u{102}', "a"),
    ('\u{103}', "a"),
    ('\u{104}', "a"),
    ('\u{105}', "a"),
    ('\u{106}', "c"),
    ('\u{107}', "c"),
    ('\u{108}', "c"),
    ('\u{109}', "c"),
    ('\u{10a}', "c"),
    ('\u{10b}', "c"),
    ('\u{10c}', "c"),
    ('\u{10d}', "c"),
    ('\u{10e}', "d"),
    ('\u{10f}', "d"),
    ('\u{112}', "e"),
    ('\u{113}', "e"),
    ('\u{114}', "e"),
    ('\u{115}', "e"),
    ('\u{116}', "e"),
    ('\u{117}', "e"),
    ('\u{118}', "e"),
    ('\u{119}', "e"),
    ('\u{11a}', "e"),
    ('\u{11b}', "e"),
    ('\u{11c}', "g"),
    ('\u{11d}', "g"),
    ('\u{11e}', "g"),
    ('\u{11f}', "g"),
    ('\u{120}', "g"),
    ('\u{121}', "g"),
    ('\u{122}', "g"),
    ('\u{123}', "g"),
    ('\u{124}', "h"),
    ('\u{125}', "h"),
    ('\u{128}', "i"),
    ('\u{129}', "i"),
    ('\u{12a}', "i"),
    ('\u{12b}', "i"),
    ('\u{12c}', "i"),
    ('\u{12d}', "i"),
    ('\u{12e}', "i"),
    ('\u{12f}', "i"),
    ('\u{130}', "i"),
    ('\u{134}', "j"),
    ('\u{135}', "j"),
    ('\u{136}', "k"),
    ('\u{137}', "k"),
    ('\u{139}', "l"),
    ('\u{13a}', "l"),
    ('\u{13b}', "l"),
    ('\u{13c}', "l"),
    ('\u{13d}', "l"),
    ('\u{13e}', "l"),
    ('\u{143}', "n"),
    ('\u{144}', "n"),
    ('\u{145}', "n"),
    ('\u{146}', "n"),
    ('\u{147}', "n"),
    ('\u{148}', "n"),
    ('\u{14c}', "o"),
    ('\u{14d}', "o"),
    ('\u{14e}', "o"),
    ('\u{14f}', "o"),
    ('\u{150}', "o"),
    ('\u{151}', "o"),
    ('\u{154}', "r"),
    ('\u{155}', "r"),
    ('\u{156}', "r"),
    ('\u{157}', "r"),
    ('\u{158}', "r"),
    ('\u{159}', "r"),
    ('\u{15a}', "s"),
    ('\u{15b}', "s"),
    ('\u{15c}', "s"),
    ('\u{15d}', "s"),
    ('\u{15e}', "s"),
    ('\u{15f}', "s"),
    ('\u{160}', "s"),
    ('\u{161}', "s"),
    ('\u{162}', "t"),
    ('\u{163}', "t"),
    ('\u{164}', "t"),
    ('\u{165}', "t"),
    ('\u{168}', "u"),
    ('\u{169}', "u"),
    ('\u{16a}', "u"),
    ('\u{16b}', "u"),
    ('\u{16c}', "u"),
    ('\u{16d}', "u"),
    ('\u{16e}', "u"),
    ('\u{16f}', "u"),
    ('\u{170}', "u"),
    ('\u{171}', "u"),
    ('\u{172}', "u"),
    ('\u{173}', "u"),
    ('\u{174}', "w"),
    ('\u{175}', "w"),
    ('\u{176}', "y"),
    ('\u{177}', "y"),
    ('\u{178}', "y"),
    ('\u{179}', "z"),
    ('\u{17a}', "z"),
    ('\u{17b}', "z"),
    ('\u{17c}', "z"),
    ('\u{17d}', "z"),
    ('\u{17e}', "z"),
    ('\u{17f}', "s"),
];

/// The private-use block a *ranked* fold ends with, in the three tables below.
///
/// `ł` under `utf8mb4_unicode_ci` is a letter of its own: MySQL sorts it with
/// the Ls, between `l` and `m`, and calls it equal to nothing else. Folding it
/// to its own lower case gets the equality right and the *order* wrong — it
/// lands after `z`, so `Łukasz` sorts to the end of a name list instead of
/// under L. So a character with no base folds to the previous base letter plus
/// one character from this block, numbered by how many such classes MySQL puts
/// between that letter and the next: `l\u{e000}` for `Ŀ` and `l\u{e001}` for
/// `Ł`, which sort after `l`, in that order, and before `m`.
///
/// Private use because no *text* can be expected to contain one, so a ranked
/// fold cannot collide with a character somebody actually stored — and because
/// the block is above every letter, so a ranked class sorts after the base
/// letter it hangs off and before the next one.
///
/// The whole of it, so the shape can be checked rather than assumed. A rank is
/// numbered from the bottom and never gets near the top.
#[cfg(test)]
const RANK_MARKERS: std::ops::RangeInclusive<char> = '\u{e000}'..='\u{f8ff}';

/// Where `utf8mb4_0900_ai_ci` folds a character the other collations do not.
///
/// UCA 9.0.0, which has expansions the older table does not: `Æ` is `ae` and
/// `Ø` is `o` here, and both are letters of their own under
/// `utf8mb4_unicode_ci`. Every entry measured on 8.3.
const FOLD_AI_CI: &[(char, &str)] = &[
    ('\u{aa}', "a"),
    ('\u{b5}', "z\u{e002}"),
    ('\u{ba}', "o"),
    ('\u{c6}', "ae"),
    ('\u{d0}', "d"),
    ('\u{d8}', "o"),
    ('\u{de}', "z\u{e000}"),
    ('\u{df}', "ss"),
    ('\u{e6}', "ae"),
    ('\u{f0}', "d"),
    ('\u{f8}', "o"),
    ('\u{fe}', "z\u{e000}"),
    ('\u{110}', "d"),
    ('\u{111}', "d"),
    ('\u{126}', "h"),
    ('\u{127}', "h"),
    ('\u{131}', "ij\u{e000}"),
    ('\u{132}', "ij"),
    ('\u{133}', "ij"),
    ('\u{138}', "q\u{e000}"),
    ('\u{13f}', "l"),
    ('\u{140}', "l"),
    ('\u{141}', "l"),
    ('\u{142}', "l"),
    ('\u{149}', "z\u{e001}"),
    ('\u{14a}', "n\u{e000}"),
    ('\u{14b}', "n\u{e000}"),
    ('\u{152}', "oe"),
    ('\u{153}', "oe"),
    ('\u{166}', "t\u{e000}"),
    ('\u{167}', "t\u{e000}"),
];

/// Where `utf8mb4_unicode_ci` folds a character the other collations do not.
///
/// UCA 4.0.0: `ß` and `Œ` expand, while `Æ`, `Ø`, `Ð`, `Đ`, `Ħ` and `Ł` are
/// letters in their own right and take a rank each — see [`RANK_MARKER`].
const FOLD_UNICODE_CI: &[(char, &str)] = &[
    ('\u{aa}', "a"),
    ('\u{b5}', "z\u{e002}"),
    ('\u{ba}', "o"),
    ('\u{c6}', "a\u{e000}"),
    ('\u{d0}', "d\u{e001}"),
    ('\u{d8}', "oe\u{e000}"),
    ('\u{de}', "z\u{e000}"),
    ('\u{df}', "ss"),
    ('\u{e6}', "a\u{e000}"),
    ('\u{f0}', "d\u{e001}"),
    ('\u{f8}', "oe\u{e000}"),
    ('\u{fe}', "z\u{e000}"),
    ('\u{110}', "d\u{e000}"),
    ('\u{111}', "d\u{e000}"),
    ('\u{126}', "h\u{e000}"),
    ('\u{127}', "h\u{e000}"),
    ('\u{131}', "ij\u{e000}"),
    ('\u{132}', "ij"),
    ('\u{133}', "ij"),
    ('\u{138}', "q\u{e000}"),
    ('\u{13f}', "l\u{e000}"),
    ('\u{140}', "l\u{e000}"),
    ('\u{141}', "l\u{e001}"),
    ('\u{142}', "l\u{e001}"),
    ('\u{149}', "z\u{e001}"),
    ('\u{14a}', "n\u{e000}"),
    ('\u{14b}', "n\u{e000}"),
    ('\u{152}', "oe"),
    ('\u{153}', "oe"),
    ('\u{166}', "t\u{e000}"),
    ('\u{167}', "t\u{e000}"),
];

/// Where `utf8mb4_general_ci` folds a character the other collations do not.
///
/// It has no expansions at all, so `ß` is a single `s` — which is why `WHERE
/// name = 'strasse'` finds `Straße` under the other two `_ci` collations and
/// not under this one. Nor does it place a letter it has no base for anywhere
/// near that base: everything from `ª` to `ŧ` sorts *after* `z` there, which
/// is why so many of these are ranked off `z`.
const FOLD_GENERAL_CI: &[(char, &str)] = &[
    ('\u{aa}', "z\u{e000}"),
    ('\u{b5}', "z\u{e010}"),
    ('\u{ba}', "z\u{e001}"),
    ('\u{c6}', "z\u{e002}"),
    ('\u{d0}', "z\u{e003}"),
    ('\u{d8}', "z\u{e004}"),
    ('\u{de}', "z\u{e005}"),
    ('\u{df}', "s"),
    ('\u{e6}', "z\u{e002}"),
    ('\u{f0}', "z\u{e003}"),
    ('\u{f8}', "z\u{e004}"),
    ('\u{fe}', "z\u{e005}"),
    ('\u{110}', "z\u{e006}"),
    ('\u{111}', "z\u{e006}"),
    ('\u{126}', "z\u{e007}"),
    ('\u{127}', "z\u{e007}"),
    ('\u{131}', "i"),
    ('\u{132}', "z\u{e008}"),
    ('\u{133}', "z\u{e008}"),
    ('\u{138}', "z\u{e009}"),
    ('\u{13f}', "z\u{e00a}"),
    ('\u{140}', "z\u{e00a}"),
    ('\u{141}', "z\u{e00b}"),
    ('\u{142}', "z\u{e00b}"),
    ('\u{149}', "z\u{e00c}"),
    ('\u{14a}', "z\u{e00d}"),
    ('\u{14b}', "z\u{e00d}"),
    ('\u{152}', "z\u{e00e}"),
    ('\u{153}', "z\u{e00e}"),
    ('\u{166}', "z\u{e00f}"),
    ('\u{167}', "z\u{e00f}"),
];

/// How firmly a value holds its collation, which decides whose wins.
///
/// MySQL's numbers, and the order is the rule: the *lower* coercibility wins,
/// so a column beats a literal and an explicit `COLLATE` beats both. Measured
/// on 8.3 — `COERCIBILITY('x')` is 4, `COERCIBILITY(col)` is 2 and a
/// `COLLATE` clause is 0 — and it is what makes a search against a
/// `utf8mb4_unicode_ci` column compare in *that* collation rather than in the
/// connection's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Coercibility {
    /// A `COLLATE` clause named it. 0 on 8.3.
    Explicit,
    /// A column's declaration carries it. 2 on 8.3.
    Implicit,
    /// A literal, which takes whichever collation it is compared against. 4.
    Coercible,
}

impl Coercibility {
    /// The word 8.3 puts in the 1267 message.
    fn word(self) -> &'static str {
        match self {
            Coercibility::Explicit => "EXPLICIT",
            Coercibility::Implicit => "IMPLICIT",
            Coercibility::Coercible => "COERCIBLE",
        }
    }
}

/// A collation and how firmly a value holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Collated {
    pub collation: Collation,
    pub coercibility: Coercibility,
}

impl Collated {
    /// What a literal, and anything computed from one, carries.
    ///
    /// The server's default collation at the weakest coercibility, so any
    /// column it meets decides the comparison. `SELECT COLLATION('x')` is
    /// `utf8mb4_0900_ai_ci` on 8.3 and `COERCIBILITY('x')` is 4.
    pub const COERCIBLE: Collated = Collated {
        collation: Collation::Ai0900Ci,
        coercibility: Coercibility::Coercible,
    };

    /// What a value read out of a column carries.
    pub fn implicit(collation: Collation) -> Self {
        Collated {
            collation,
            coercibility: Coercibility::Implicit,
        }
    }

    /// What a `COLLATE` clause puts on its operand.
    pub fn explicit(collation: Collation) -> Self {
        Collated {
            collation,
            coercibility: Coercibility::Explicit,
        }
    }
}

impl Default for Collated {
    fn default() -> Self {
        Collated::COERCIBLE
    }
}

/// The collation a comparison of two operands is made under, or `None` where
/// MySQL refuses to pick one.
///
/// Three rules, all measured on 8.3 against two columns of a table declared
/// with four different collations:
///
/// * the lower coercibility wins — a `utf8mb4_unicode_ci` column against a
///   literal compares as `utf8mb4_unicode_ci`, which is the rule the whole of
///   #0152 turns on;
/// * a `_bin` collation wins over a `_ci` one of the same character set even
///   at equal coercibility, so `bin_col = unicode_col` compares as bytes
///   rather than raising;
/// * anything else at equal coercibility and different collations is 1267.
///   Measured both ways: two IMPLICIT columns
///   (`utf8mb4_0900_ai_ci` against `utf8mb4_unicode_ci`) raise it, and so do
///   two EXPLICIT `COLLATE` clauses that disagree.
pub fn resolve(a: Collated, b: Collated) -> Option<Collation> {
    match a.coercibility.cmp(&b.coercibility) {
        Ordering::Less => return Some(a.collation),
        Ordering::Greater => return Some(b.collation),
        Ordering::Equal => {}
    }
    if a.collation == b.collation {
        return Some(a.collation);
    }
    if a.collation == Collation::Bin || b.collation == Collation::Bin {
        return Some(Collation::Bin);
    }
    None
}

/// 8.3's answer to a pair of collations it will not pick between.
///
/// Wording and code alike: `Illegal mix of collations
/// (utf8mb4_0900_ai_ci,IMPLICIT) and (utf8mb4_unicode_ci,IMPLICIT) for
/// operation '='`.
pub fn illegal_mix(a: Collated, b: Collated, operation: &str) -> EngineError {
    EngineError::new(
        1267,
        "HY000",
        format!(
            "Illegal mix of collations ({}) and ({}) for operation '{operation}'",
            named_pair(a),
            named_pair(b),
        ),
    )
}

/// The same refusal over three operands, which carries a different code.
///
/// 1270 rather than 1267, and the three are listed with commas rather than
/// joined by `and`. Both measured on 8.3 against `u BETWEEN g AND g` over two
/// `_ci` columns — the only construct in this engine that resolves three
/// operands at once.
pub fn illegal_mix_of_three(a: Collated, b: Collated, c: Collated, operation: &str) -> EngineError {
    EngineError::new(
        1270,
        "HY000",
        format!(
            "Illegal mix of collations ({}), ({}), ({}) for operation '{operation}'",
            named_pair(a),
            named_pair(b),
            named_pair(c),
        ),
    )
}

/// One operand as 8.3 names it in those two messages.
fn named_pair(c: Collated) -> String {
    format!("{},{}", c.collation.name(), c.coercibility.word())
}

/// A collation this engine has.
pub struct Known {
    pub name: &'static str,
    pub charset: &'static str,
    /// The id MySQL gives it, which `information_schema.COLLATIONS` reports.
    pub id: i64,
    pub is_default: bool,
    pub sortlen: i64,
}

/// Every collation this engine has.
///
/// Deliberately short, and every one of them a comparison this engine actually
/// makes — [`Collation`] is the same list as behaviour.
/// `information_schema.COLLATIONS` is built from this, and a `COLLATE` clause
/// is checked against it, so the two cannot claim different things.
///
/// A collation MySQL has and this does not — `latin1_swedish_ci`,
/// `utf8mb3_general_ci` — is refused rather than accepted and treated as
/// utf8mb4. Refused as *unsupported* and never as unknown: it is a real
/// collation, and [`MYSQL_COLLATIONS`] is what keeps the two apart.
pub const KNOWN: &[Known] = &[
    Known {
        name: "utf8mb4_0900_ai_ci",
        charset: "utf8mb4",
        id: 255,
        is_default: true,
        sortlen: 0,
    },
    Known {
        name: "utf8mb4_general_ci",
        charset: "utf8mb4",
        id: 45,
        is_default: false,
        sortlen: 1,
    },
    Known {
        name: "utf8mb4_unicode_ci",
        charset: "utf8mb4",
        id: 224,
        is_default: false,
        sortlen: 8,
    },
    // Read off `information_schema.COLLATIONS` on a stock 8.3, like its
    // neighbours. Honoured since the engine gained a comparison per collation
    // rather than one for all of them: recording the name and then comparing
    // the column case-insensitively anyway was the wrong answer that kept it
    // out of this list.
    Known {
        name: "utf8mb4_bin",
        charset: "utf8mb4",
        id: 46,
        is_default: false,
        sortlen: 1,
    },
    Known {
        name: "binary",
        charset: "binary",
        id: 63,
        is_default: true,
        sortlen: 1,
    },
];

/// The collation MySQL declares its own `information_schema` text columns
/// with, and the only one this engine reports for a binary-collated column.
///
/// Not in [`KNOWN`]: no `COLLATE` clause may name it — 8.3 has it, this engine
/// cannot compare in it, and the only columns that carry it are the ones
/// `information_schema` declares. `SqlType::binary_collation` is the flag, and
/// this is the name that goes with it.
pub const BINARY: &str = "utf8mb3_bin";

/// Every collation MySQL 8.3 has, which is what makes 1273 exact.
///
/// Read off `information_schema.COLLATIONS` on a stock `mysql:8.3`, lowercased
/// and sorted so the lookup is a binary search. Measured data rather than a
/// judgement, and it answers one question only: whether a name is a collation
/// *at all*. Which of them this engine can honour is [`KNOWN`], a far shorter
/// list.
///
/// Without it the boundary cannot be drawn. A `_ci` suffix alone blessed
/// `bogus_ci`; [`KNOWN`] alone called `utf8mb4_bin` unknown, which is a false
/// claim about a name 8.3 has and this engine merely cannot compare in.
pub const MYSQL_COLLATIONS: &[&str] = &[
    "armscii8_bin",
    "armscii8_general_ci",
    "ascii_bin",
    "ascii_general_ci",
    "big5_bin",
    "big5_chinese_ci",
    "binary",
    "cp1250_bin",
    "cp1250_croatian_ci",
    "cp1250_czech_cs",
    "cp1250_general_ci",
    "cp1250_polish_ci",
    "cp1251_bin",
    "cp1251_bulgarian_ci",
    "cp1251_general_ci",
    "cp1251_general_cs",
    "cp1251_ukrainian_ci",
    "cp1256_bin",
    "cp1256_general_ci",
    "cp1257_bin",
    "cp1257_general_ci",
    "cp1257_lithuanian_ci",
    "cp850_bin",
    "cp850_general_ci",
    "cp852_bin",
    "cp852_general_ci",
    "cp866_bin",
    "cp866_general_ci",
    "cp932_bin",
    "cp932_japanese_ci",
    "dec8_bin",
    "dec8_swedish_ci",
    "eucjpms_bin",
    "eucjpms_japanese_ci",
    "euckr_bin",
    "euckr_korean_ci",
    "gb18030_bin",
    "gb18030_chinese_ci",
    "gb18030_unicode_520_ci",
    "gb2312_bin",
    "gb2312_chinese_ci",
    "gbk_bin",
    "gbk_chinese_ci",
    "geostd8_bin",
    "geostd8_general_ci",
    "greek_bin",
    "greek_general_ci",
    "hebrew_bin",
    "hebrew_general_ci",
    "hp8_bin",
    "hp8_english_ci",
    "keybcs2_bin",
    "keybcs2_general_ci",
    "koi8r_bin",
    "koi8r_general_ci",
    "koi8u_bin",
    "koi8u_general_ci",
    "latin1_bin",
    "latin1_danish_ci",
    "latin1_general_ci",
    "latin1_general_cs",
    "latin1_german1_ci",
    "latin1_german2_ci",
    "latin1_spanish_ci",
    "latin1_swedish_ci",
    "latin2_bin",
    "latin2_croatian_ci",
    "latin2_czech_cs",
    "latin2_general_ci",
    "latin2_hungarian_ci",
    "latin5_bin",
    "latin5_turkish_ci",
    "latin7_bin",
    "latin7_estonian_cs",
    "latin7_general_ci",
    "latin7_general_cs",
    "macce_bin",
    "macce_general_ci",
    "macroman_bin",
    "macroman_general_ci",
    "sjis_bin",
    "sjis_japanese_ci",
    "swe7_bin",
    "swe7_swedish_ci",
    "tis620_bin",
    "tis620_thai_ci",
    "ucs2_bin",
    "ucs2_croatian_ci",
    "ucs2_czech_ci",
    "ucs2_danish_ci",
    "ucs2_esperanto_ci",
    "ucs2_estonian_ci",
    "ucs2_general_ci",
    "ucs2_general_mysql500_ci",
    "ucs2_german2_ci",
    "ucs2_hungarian_ci",
    "ucs2_icelandic_ci",
    "ucs2_latvian_ci",
    "ucs2_lithuanian_ci",
    "ucs2_persian_ci",
    "ucs2_polish_ci",
    "ucs2_roman_ci",
    "ucs2_romanian_ci",
    "ucs2_sinhala_ci",
    "ucs2_slovak_ci",
    "ucs2_slovenian_ci",
    "ucs2_spanish2_ci",
    "ucs2_spanish_ci",
    "ucs2_swedish_ci",
    "ucs2_turkish_ci",
    "ucs2_unicode_520_ci",
    "ucs2_unicode_ci",
    "ucs2_vietnamese_ci",
    "ujis_bin",
    "ujis_japanese_ci",
    "utf16_bin",
    "utf16_croatian_ci",
    "utf16_czech_ci",
    "utf16_danish_ci",
    "utf16_esperanto_ci",
    "utf16_estonian_ci",
    "utf16_general_ci",
    "utf16_german2_ci",
    "utf16_hungarian_ci",
    "utf16_icelandic_ci",
    "utf16_latvian_ci",
    "utf16_lithuanian_ci",
    "utf16_persian_ci",
    "utf16_polish_ci",
    "utf16_roman_ci",
    "utf16_romanian_ci",
    "utf16_sinhala_ci",
    "utf16_slovak_ci",
    "utf16_slovenian_ci",
    "utf16_spanish2_ci",
    "utf16_spanish_ci",
    "utf16_swedish_ci",
    "utf16_turkish_ci",
    "utf16_unicode_520_ci",
    "utf16_unicode_ci",
    "utf16_vietnamese_ci",
    "utf16le_bin",
    "utf16le_general_ci",
    "utf32_bin",
    "utf32_croatian_ci",
    "utf32_czech_ci",
    "utf32_danish_ci",
    "utf32_esperanto_ci",
    "utf32_estonian_ci",
    "utf32_general_ci",
    "utf32_german2_ci",
    "utf32_hungarian_ci",
    "utf32_icelandic_ci",
    "utf32_latvian_ci",
    "utf32_lithuanian_ci",
    "utf32_persian_ci",
    "utf32_polish_ci",
    "utf32_roman_ci",
    "utf32_romanian_ci",
    "utf32_sinhala_ci",
    "utf32_slovak_ci",
    "utf32_slovenian_ci",
    "utf32_spanish2_ci",
    "utf32_spanish_ci",
    "utf32_swedish_ci",
    "utf32_turkish_ci",
    "utf32_unicode_520_ci",
    "utf32_unicode_ci",
    "utf32_vietnamese_ci",
    "utf8mb3_bin",
    "utf8mb3_croatian_ci",
    "utf8mb3_czech_ci",
    "utf8mb3_danish_ci",
    "utf8mb3_esperanto_ci",
    "utf8mb3_estonian_ci",
    "utf8mb3_general_ci",
    "utf8mb3_general_mysql500_ci",
    "utf8mb3_german2_ci",
    "utf8mb3_hungarian_ci",
    "utf8mb3_icelandic_ci",
    "utf8mb3_latvian_ci",
    "utf8mb3_lithuanian_ci",
    "utf8mb3_persian_ci",
    "utf8mb3_polish_ci",
    "utf8mb3_roman_ci",
    "utf8mb3_romanian_ci",
    "utf8mb3_sinhala_ci",
    "utf8mb3_slovak_ci",
    "utf8mb3_slovenian_ci",
    "utf8mb3_spanish2_ci",
    "utf8mb3_spanish_ci",
    "utf8mb3_swedish_ci",
    "utf8mb3_tolower_ci",
    "utf8mb3_turkish_ci",
    "utf8mb3_unicode_520_ci",
    "utf8mb3_unicode_ci",
    "utf8mb3_vietnamese_ci",
    "utf8mb4_0900_ai_ci",
    "utf8mb4_0900_as_ci",
    "utf8mb4_0900_as_cs",
    "utf8mb4_0900_bin",
    "utf8mb4_bg_0900_ai_ci",
    "utf8mb4_bg_0900_as_cs",
    "utf8mb4_bin",
    "utf8mb4_bs_0900_ai_ci",
    "utf8mb4_bs_0900_as_cs",
    "utf8mb4_croatian_ci",
    "utf8mb4_cs_0900_ai_ci",
    "utf8mb4_cs_0900_as_cs",
    "utf8mb4_czech_ci",
    "utf8mb4_da_0900_ai_ci",
    "utf8mb4_da_0900_as_cs",
    "utf8mb4_danish_ci",
    "utf8mb4_de_pb_0900_ai_ci",
    "utf8mb4_de_pb_0900_as_cs",
    "utf8mb4_eo_0900_ai_ci",
    "utf8mb4_eo_0900_as_cs",
    "utf8mb4_es_0900_ai_ci",
    "utf8mb4_es_0900_as_cs",
    "utf8mb4_es_trad_0900_ai_ci",
    "utf8mb4_es_trad_0900_as_cs",
    "utf8mb4_esperanto_ci",
    "utf8mb4_estonian_ci",
    "utf8mb4_et_0900_ai_ci",
    "utf8mb4_et_0900_as_cs",
    "utf8mb4_general_ci",
    "utf8mb4_german2_ci",
    "utf8mb4_gl_0900_ai_ci",
    "utf8mb4_gl_0900_as_cs",
    "utf8mb4_hr_0900_ai_ci",
    "utf8mb4_hr_0900_as_cs",
    "utf8mb4_hu_0900_ai_ci",
    "utf8mb4_hu_0900_as_cs",
    "utf8mb4_hungarian_ci",
    "utf8mb4_icelandic_ci",
    "utf8mb4_is_0900_ai_ci",
    "utf8mb4_is_0900_as_cs",
    "utf8mb4_ja_0900_as_cs",
    "utf8mb4_ja_0900_as_cs_ks",
    "utf8mb4_la_0900_ai_ci",
    "utf8mb4_la_0900_as_cs",
    "utf8mb4_latvian_ci",
    "utf8mb4_lithuanian_ci",
    "utf8mb4_lt_0900_ai_ci",
    "utf8mb4_lt_0900_as_cs",
    "utf8mb4_lv_0900_ai_ci",
    "utf8mb4_lv_0900_as_cs",
    "utf8mb4_mn_cyrl_0900_ai_ci",
    "utf8mb4_mn_cyrl_0900_as_cs",
    "utf8mb4_nb_0900_ai_ci",
    "utf8mb4_nb_0900_as_cs",
    "utf8mb4_nn_0900_ai_ci",
    "utf8mb4_nn_0900_as_cs",
    "utf8mb4_persian_ci",
    "utf8mb4_pl_0900_ai_ci",
    "utf8mb4_pl_0900_as_cs",
    "utf8mb4_polish_ci",
    "utf8mb4_ro_0900_ai_ci",
    "utf8mb4_ro_0900_as_cs",
    "utf8mb4_roman_ci",
    "utf8mb4_romanian_ci",
    "utf8mb4_ru_0900_ai_ci",
    "utf8mb4_ru_0900_as_cs",
    "utf8mb4_sinhala_ci",
    "utf8mb4_sk_0900_ai_ci",
    "utf8mb4_sk_0900_as_cs",
    "utf8mb4_sl_0900_ai_ci",
    "utf8mb4_sl_0900_as_cs",
    "utf8mb4_slovak_ci",
    "utf8mb4_slovenian_ci",
    "utf8mb4_spanish2_ci",
    "utf8mb4_spanish_ci",
    "utf8mb4_sr_latn_0900_ai_ci",
    "utf8mb4_sr_latn_0900_as_cs",
    "utf8mb4_sv_0900_ai_ci",
    "utf8mb4_sv_0900_as_cs",
    "utf8mb4_swedish_ci",
    "utf8mb4_tr_0900_ai_ci",
    "utf8mb4_tr_0900_as_cs",
    "utf8mb4_turkish_ci",
    "utf8mb4_unicode_520_ci",
    "utf8mb4_unicode_ci",
    "utf8mb4_vi_0900_ai_ci",
    "utf8mb4_vi_0900_as_cs",
    "utf8mb4_vietnamese_ci",
    "utf8mb4_zh_0900_as_cs",
];

/// The collation a `COLLATE` clause names, as MySQL records it.
///
/// The name as written rather than its rendering, and lowercased, because 8.3
/// lowercases the name it records: `COLLATE UTF8MB4_UNICODE_CI` is
/// `utf8mb4_unicode_ci` in `COLLATION_NAME`, whatever case it was written in.
pub fn read(name: &ObjectName) -> EngineResult<String> {
    named(&name::dotted(name))
}

/// The same, from a table option, where the value may be written as a literal.
///
/// `COLLATE = 'utf8mb4_unicode_ci'` and `COLLATE utf8mb4_unicode_ci` are the
/// same clause, so the literal's *value* is read rather than its rendering —
/// which would keep the quotes and then fail to match any known name.
pub fn read_option(value: &Expr) -> EngineResult<String> {
    match value {
        Expr::Identifier(ident) => named(&ident.value),
        Expr::CompoundIdentifier(parts) => {
            named(&parts.last().map(|p| p.value.clone()).unwrap_or_default())
        }
        Expr::Value(v) => match &v.value {
            sqlparser::ast::Value::SingleQuotedString(s)
            | sqlparser::ast::Value::DoubleQuotedString(s) => named(s),
            other => Err(unknown(&other.to_string())),
        },
        other => Err(unknown(&other.to_string())),
    }
}

/// Checks one name and hands back the spelling MySQL would record.
///
/// Two refusals, and they are different questions. A name no collation has is
/// 1273, exactly as 8.3 answers it. A collation MySQL *has* and this engine
/// cannot honour — `latin1_swedish_ci`, `utf8mb4_0900_as_cs` — is the
/// unsupported refusal, because the statement is well formed and the gap is
/// this engine's. Recording such a name and then comparing the column the way
/// every other one is compared is the wrong answer this refusal exists to
/// prevent, and the reason the accepted list is [`Collation`] rather than
/// every `_ci` suffix: a name is accepted here exactly when there is a
/// comparison behind it.
pub fn named(name: &str) -> EngineResult<String> {
    let name = name.to_ascii_lowercase();
    if Collation::from_name(&name).is_some() {
        return Ok(name);
    }
    // A collation 8.3 has and this engine cannot honour, against a name that
    // is not a collation anywhere. The first is this engine's gap and the
    // second is the statement's, and they are not the same error.
    if MYSQL_COLLATIONS.binary_search(&name.as_str()).is_ok() {
        return Err(unsupported(format!("the `{name}` collation")));
    }
    Err(unknown(&name))
}

/// The collation a column ends up with, given the clauses in scope.
///
/// 8.3's inheritance, narrowest first: a column's own `COLLATE` wins over its
/// table's, and the table's over the database's. All three measured — a
/// table-level clause shows up on every character column of the table, and a
/// database-level one on every character column of every table that declares
/// nothing.
///
/// One function because two readers of this rule is how the *reported*
/// collation and the *compared* one would come to disagree:
/// `information_schema.COLUMNS.COLLATION_NAME` and
/// [`crate::types::SqlType::collation`] are the same fact asked twice.
pub fn inherited<'a>(
    column: Option<&'a str>,
    table: Option<&'a str>,
    database: &'a str,
) -> &'a str {
    column.or(table).unwrap_or(database)
}

/// 8.3's answer to a collation it does not have, code and wording alike.
fn unknown(name: &str) -> EngineError {
    EngineError::new(1273, "HY000", format!("Unknown collation: '{name}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_case_insensitive_collation_is_accepted_and_lowercased() {
        assert_eq!(named("utf8mb4_unicode_ci").unwrap(), "utf8mb4_unicode_ci");
        assert_eq!(named("UTF8MB4_UNICODE_CI").unwrap(), "utf8mb4_unicode_ci");
    }

    /// The two refusals are different questions and carry different codes.
    #[test]
    fn an_unknown_name_is_1273_and_a_known_one_this_engine_cannot_honour_is_not() {
        let unknown = named("bogus_ci").unwrap_err();
        assert_eq!(unknown.code, 1273);
        assert!(
            unknown
                .to_string()
                .contains("Unknown collation: 'bogus_ci'"),
            "{unknown}"
        );
        // Real on 8.3 and not honourable here: this engine's gap, not a
        // claim about the name, so never 1273. `binary` is in [`KNOWN`] and
        // still refused, because it is the binary *character set* rather than
        // a comparison over characters — `Value::Bytes` is what carries that.
        for real in [
            "latin1_swedish_ci",
            "binary",
            "utf8mb4_0900_as_cs",
            "utf8mb4_unicode_520_ci",
        ] {
            assert!(
                MYSQL_COLLATIONS.binary_search(&real).is_ok(),
                "{real} should be in the measured list"
            );
            assert_ne!(named(real).unwrap_err().code, 1273, "{real}");
        }
    }

    /// The list `information_schema.COLLATIONS` is built from is the list a
    /// clause is checked against; nothing may be in one and not the other.
    #[test]
    fn every_known_collation_is_reportable() {
        assert!(!KNOWN.is_empty());
        for k in KNOWN {
            assert_eq!(k.name, k.name.to_ascii_lowercase(), "{}", k.name);
            assert!(!k.charset.is_empty(), "{}", k.name);
            // Nothing may be offered that MySQL does not have, or the engine
            // would accept a clause 8.3 answers 1273 to.
            assert!(
                MYSQL_COLLATIONS.binary_search(&k.name).is_ok(),
                "{} is not a MySQL collation",
                k.name
            );
        }
    }

    /// The fold tables are binary-searched too, and an entry out of order
    /// there is worse than one in [`MYSQL_COLLATIONS`]: the character would
    /// silently fall through to its own lower case and compare accent-
    /// sensitively, which is the bug this whole module exists to fix.
    #[test]
    fn the_fold_tables_are_sorted_and_disjoint() {
        for table in [FOLD, FOLD_AI_CI, FOLD_UNICODE_CI, FOLD_GENERAL_CI] {
            assert!(table.windows(2).all(|w| w[0].0 < w[1].0));
            assert!(table.iter().all(|(_, base)| !base.is_empty()));
            // A fold is ASCII letters, optionally followed by one rank
            // marker. Anything else — a stray letter after the marker, a
            // marker in the middle — would sort a class into the wrong place
            // rather than fail to compile.
            for (c, folded) in table {
                let ranked = |c: char| RANK_MARKERS.contains(&c);
                let (letters, rank) = match folded.strip_suffix(ranked) {
                    Some(letters) => (letters, true),
                    None => (*folded, false),
                };
                assert!(
                    letters.chars().all(|c| c.is_ascii_lowercase()),
                    "{c:?} folds to {folded:?}"
                );
                assert!(!letters.is_empty() || !rank, "{c:?} is ranked off nothing");
            }
        }
        // A character in the shared table *and* an override would be decided
        // by lookup order rather than by the measurement, so the two tables
        // are kept apart instead: everything the three collations disagreed
        // about was left out of `FOLD`.
        for table in [FOLD_AI_CI, FOLD_UNICODE_CI, FOLD_GENERAL_CI] {
            for (c, _) in table {
                assert!(lookup(FOLD, *c).is_none(), "{c:?} is in both tables");
            }
        }
    }

    /// Every collation the enum has is offerable and every offered utf8mb4
    /// name resolves to one, so a `COLLATE` clause cannot name a collation
    /// `information_schema.COLLATIONS` denies or vice versa.
    #[test]
    fn the_enum_and_the_reported_list_agree() {
        for name in [
            "utf8mb4_0900_ai_ci",
            "utf8mb4_unicode_ci",
            "utf8mb4_general_ci",
            "utf8mb4_bin",
        ] {
            let collation = Collation::from_name(name).expect(name);
            assert_eq!(collation.name(), name);
            assert!(KNOWN.iter().any(|k| k.name == name), "{name}");
        }
        assert_eq!(Collation::from_name("utf8mb4_0900_as_cs"), None);
    }

    /// Each of the four answers the measured cases differently, which is the
    /// whole point: one comparison wearing four names is three wrong answers.
    #[test]
    fn each_collation_answers_the_measured_cases() {
        // rows 'café', 'Cafe', 'Straße' against 'cafe' and 'strasse', from
        // the note on #0152.
        for (collation, cafe, strasse, case) in [
            (Collation::Ai0900Ci, true, true, true),
            (Collation::UnicodeCi, true, true, true),
            (Collation::GeneralCi, true, false, true),
            (Collation::Bin, false, false, false),
        ] {
            assert_eq!(collation.same("café", "cafe"), cafe, "{collation:?} café");
            assert_eq!(
                collation.same("Straße", "strasse"),
                strasse,
                "{collation:?} Straße"
            );
            assert_eq!(collation.same("Cafe", "cafe"), case, "{collation:?} Cafe");
        }
        // `utf8mb4_general_ci` folds `ß` to a single `s`, which is why it
        // finds `Straße` for `strase` and the other two do not.
        assert!(Collation::GeneralCi.same("Straße", "strase"));
        assert!(!Collation::Ai0900Ci.same("Straße", "strase"));
        // Only `utf8mb4_0900_ai_ci` has the `æ` and `ø` expansions.
        assert!(Collation::Ai0900Ci.same("æ", "ae"));
        assert!(!Collation::UnicodeCi.same("æ", "ae"));
        assert!(Collation::Ai0900Ci.same("ø", "o"));
        assert!(!Collation::UnicodeCi.same("ø", "o"));
    }

    /// A letter with no base of its own still sorts where MySQL puts it.
    ///
    /// The half that a fold to the character's own lower case gets wrong:
    /// equality is right either way, and `Łukasz` lands after `Zielinski`
    /// instead of with the Ls. Measured on 8.3 — under
    /// `utf8mb4_unicode_ci`, `Ł` sorts between `l` and `m`, `æ` between the
    /// `a` class and `b`, and `ø` after `œ` and before `p`.
    #[test]
    fn a_letter_of_its_own_sorts_where_it_belongs() {
        let uni = Collation::UnicodeCi;
        for (smaller, larger) in [
            ("lz", "ł"),
            ("ł", "m"),
            ("az", "æ"),
            ("æ", "b"),
            ("oe", "ø"),
            ("ø", "p"),
        ] {
            assert_eq!(
                uni.compare(smaller, larger),
                Ordering::Less,
                "{smaller:?} < {larger:?}"
            );
        }
        // `utf8mb4_general_ci` places none of them near a base: everything it
        // has no fold for sorts after `z`, which is also measured.
        for after_z in ["ł", "æ", "ø", "þ"] {
            assert_eq!(
                Collation::GeneralCi.compare(after_z, "zz"),
                Ordering::Greater,
                "{after_z:?}"
            );
        }
    }

    /// A trailing space is ignored by three of the four and not by the
    /// default, and a tab is ignored by none.
    #[test]
    fn only_the_default_collation_refuses_to_pad() {
        assert!(!Collation::Ai0900Ci.same("a ", "a"));
        for collation in [Collation::UnicodeCi, Collation::GeneralCi, Collation::Bin] {
            assert!(collation.same("a ", "a"), "{collation:?}");
            assert!(!collation.same("a\t", "a"), "{collation:?}");
        }
    }

    /// A combining mark folds away under the two UCA collations only.
    #[test]
    fn a_combining_mark_is_ignored_where_the_name_says_ai() {
        let decomposed = "cafe\u{301}";
        assert!(Collation::Ai0900Ci.same(decomposed, "cafe"));
        assert!(Collation::UnicodeCi.same(decomposed, "cafe"));
        assert!(!Collation::GeneralCi.same(decomposed, "cafe"));
        assert!(!Collation::Bin.same(decomposed, "cafe"));
    }

    /// `LIKE` applies the case and accent fold and *not* the expansions,
    /// which is the pairing measured on 8.3.
    #[test]
    fn like_folds_characters_without_expanding_them() {
        assert!(Collation::Ai0900Ci.chars_match('é', 'E'));
        assert!(Collation::UnicodeCi.chars_match('é', 'E'));
        assert!(!Collation::Bin.chars_match('é', 'E'));
        // `ß` is one character to `LIKE` even where `=` reads it as two.
        assert!(Collation::Ai0900Ci.same("ß", "ss"));
        assert!(!Collation::Ai0900Ci.chars_match('ß', 's'));
        assert!(Collation::Ai0900Ci.chars_match('ß', 'ß'));
    }

    /// The key and the comparison are one rule, or `GROUP BY` splits a pair
    /// `=` calls equal.
    #[test]
    fn the_sort_key_agrees_with_the_comparison() {
        for collation in [
            Collation::Ai0900Ci,
            Collation::UnicodeCi,
            Collation::GeneralCi,
            Collation::Bin,
        ] {
            for (a, b) in [
                ("café", "cafe"),
                ("Straße", "strasse"),
                ("a ", "a"),
                ("x", "y"),
                ("æ", "ae"),
            ] {
                assert_eq!(
                    collation.key(a) == collation.key(b),
                    collation.same(a, b),
                    "{collation:?} {a:?} {b:?}"
                );
            }
        }
    }

    /// The coercion rules, all three measured against 8.3.
    #[test]
    fn the_stronger_operand_decides_the_collation() {
        let unicode = Collated::implicit(Collation::UnicodeCi);
        let default = Collated::implicit(Collation::Ai0900Ci);
        let bin = Collated::implicit(Collation::Bin);
        // A column beats a literal.
        assert_eq!(
            resolve(unicode, Collated::COERCIBLE),
            Some(Collation::UnicodeCi)
        );
        assert_eq!(
            resolve(Collated::COERCIBLE, unicode),
            Some(Collation::UnicodeCi)
        );
        // `_bin` beats a `_ci` of the same character set at equal strength.
        assert_eq!(resolve(unicode, bin), Some(Collation::Bin));
        assert_eq!(resolve(bin, default), Some(Collation::Bin));
        // Two disagreeing `_ci` columns are 1267, and so are two disagreeing
        // `COLLATE` clauses.
        assert_eq!(resolve(unicode, default), None);
        assert_eq!(
            resolve(
                Collated::explicit(Collation::UnicodeCi),
                Collated::explicit(Collation::GeneralCi)
            ),
            None
        );
        // An explicit clause beats a column, which is how a `_bin` literal
        // makes a `_ci` column compare exactly.
        assert_eq!(
            resolve(unicode, Collated::explicit(Collation::Bin)),
            Some(Collation::Bin)
        );
    }

    /// 8.3's wording, because a client matches on it.
    #[test]
    fn the_illegal_mix_message_is_8_3s() {
        let error = illegal_mix(
            Collated::implicit(Collation::Ai0900Ci),
            Collated::implicit(Collation::UnicodeCi),
            "=",
        );
        assert_eq!(error.code, 1267);
        assert!(
            error.to_string().contains(
                "Illegal mix of collations (utf8mb4_0900_ai_ci,IMPLICIT) and \
                 (utf8mb4_unicode_ci,IMPLICIT) for operation '='"
            ),
            "{error}"
        );
    }

    /// The list is searched with `binary_search`, which silently returns the
    /// wrong answer on an unsorted slice — so a name added out of order would
    /// be reported as unknown rather than failing to build.
    #[test]
    fn the_measured_list_is_sorted_and_lower_case() {
        assert_eq!(MYSQL_COLLATIONS.len(), 286);
        assert!(MYSQL_COLLATIONS.windows(2).all(|w| w[0] < w[1]));
        assert!(MYSQL_COLLATIONS
            .iter()
            .all(|c| *c == c.to_ascii_lowercase()));
        // The one this engine reports for a binary-collated column is real.
        assert!(MYSQL_COLLATIONS.binary_search(&BINARY).is_ok());
    }
}
