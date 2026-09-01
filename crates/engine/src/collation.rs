//! Which collations this engine has, and reading the name of one.
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

use sqlparser::ast::{Expr, ObjectName};

use crate::error::{unsupported, EngineError, EngineResult};
use crate::name;

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
/// Deliberately short. This engine models exactly one comparison — case
/// insensitive, accent sensitive, utf8mb4 — so a longer list would be a longer
/// list of names it treats identically. `information_schema.COLLATIONS` is
/// built from this, and a `COLLATE` clause is checked against it, so the two
/// cannot claim different things.
///
/// A collation MySQL has and this does not — `latin1_swedish_ci`,
/// `utf8mb3_general_ci` — is refused rather than accepted and treated as
/// utf8mb4, the same trade as refusing `utf8mb4_bin`. Refused as
/// *unsupported* and never as unknown: it is a real collation, and
/// [`MYSQL_COLLATIONS`] is what keeps the two apart.
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
/// cannot honour is the unsupported refusal, because the statement is well
/// formed and the gap is this engine's: comparison here is case-insensitive,
/// so a `_ci` collation is honoured by doing nothing, while `utf8mb4_bin` and
/// `latin1_swedish_ci` would compare differently. Recording one and then comparing the column the way every
/// other one is compared is the wrong answer this refusal exists to prevent —
/// measured on 8.3, a `VARCHAR(10) COLLATE utf8mb4_bin` under a UNIQUE key
/// holds `'a'` and `'A'` at once where this engine answered 1062, and
/// `WHERE a = 'A'` returns `A` there and returned `a` here.
///
/// One gap is knowingly left open, and it is why `_ci` is honoured by doing
/// nothing rather than exactly: comparison here is accent-*sensitive*, so
/// `utf8mb4_unicode_ci` folds `café` and `cafe` together in MySQL and not
/// here. That is true of every comparison this engine makes, including under
/// the server's own default collation, so refusing it would reject the schema
/// while leaving the same difference everywhere else.
pub fn named(name: &str) -> EngineResult<String> {
    let name = name.to_ascii_lowercase();
    if KNOWN
        .iter()
        .any(|k| k.name == name && k.name.ends_with("_ci"))
    {
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
        // claim about the name, so never 1273.
        for real in [
            "utf8mb4_bin",
            "latin1_swedish_ci",
            "binary",
            "utf8mb4_0900_as_cs",
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
