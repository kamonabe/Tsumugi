//! Float / NaN の決定的共有 helper（決定性 Slice 1、§4.5 / §4.4）。
//!
//! ツリーウォーク評価器（eval.rs）と VM（vm.rs）、および組み込み関数・lexer・値表示が
//! すべてここを経由することで、Float の生成・parse・format・serialize を 2 backend で
//! 同一 algorithm に揃える。[`docs/determinism-and-audit.md`] §4.5「Float と NaN」と
//! §4.4「locale と Unicode」の次の契約を実装の正本として 1 本化する。
//!
//! - 演算で生じた NaN は quiet NaN bit pattern `0x7ff8_0000_0000_0000`
//!   （[`CANONICAL_NAN_BITS`]）へ canonicalize する。
//! - `-0.0 == 0.0` は `true`。符号ビットは値として保持し、deterministic binary
//!   encoding（[`float_to_deterministic_bits`]）でのみ `+0.0` / `-0.0` を区別する。
//! - Float の parse / format は process locale を参照しない。小数点は常に `.` で、
//!   tree / VM / record-replay が同じ algorithm を共有する。
//! - 表示規則そのものは言語仕様（[`docs/language-spec.md`]）に従い、本 helper は
//!   現行 `Value::Float` の `Display` と bit-identical な文字列を返す（挙動不変）。
//!
//! 比較 / 等価の意味論（`NaN == NaN` は `false` 等）は [`crate::value::NumericOrder`] が
//! 正本であり、本モジュールはそこへ関与しない。

/// 演算で生じた NaN を畳み込む canonical quiet NaN の bit pattern（§4.5）。
///
/// IEEE 754 binary64 の quiet NaN。tree / VM / host response 由来の NaN をすべて
/// この 1 つの表現へ正規化し、backend 間や実行間で NaN の bit 表現が揺れないようにする。
pub const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

/// 演算結果の Float を決定的な表現へ正規化する（§4.5）。
///
/// `f` が NaN なら canonical quiet NaN（[`CANONICAL_NAN_BITS`]）へ畳み込む。それ以外は
/// `f` をそのまま返し、`-0.0` / `+0.0` の符号ビットや Infinity を保持する。Float を生成する
/// すべての演算サイト（tree / VM の二項・単項演算、組み込み関数）はこの関数を経由する。
pub fn canonicalize_float(f: f64) -> f64 {
    if f.is_nan() { canonical_nan() } else { f }
}

/// canonical quiet NaN（[`CANONICAL_NAN_BITS`]）を返す（§4.5）。
///
/// NaN を直接生成したい箇所（例: `min` / `max` の NaN 返却）がこの 1 つの表現を共有する。
pub fn canonical_nan() -> f64 {
    f64::from_bits(CANONICAL_NAN_BITS)
}

/// locale 非依存で Float リテラル / 文字列を parse する（§4.4）。
///
/// 小数点は常に `.` で、process locale を参照しない（`"3,14"` は Err）。成功値は
/// [`canonicalize_float`] を通し、parse 由来の NaN も canonical 表現へ揃える。lexer の
/// Float リテラルと `to_float` が同じ契約を共有するための正本。
pub fn parse_float(s: &str) -> Result<f64, std::num::ParseFloatError> {
    s.parse::<f64>().map(canonicalize_float)
}

/// Float を表示用文字列へ変換する（§4.5「表示規則は言語仕様に従う」）。
///
/// 現行の `Value::Float` の `Display`（`format!("{}", f)`）と bit-identical な文字列を返す。
/// locale 非依存で、`inf` / `-inf` / `NaN` も標準の表示をそのまま用いる。format の正本を
/// ここへ 1 本化し、tree / VM / 各表示経路が同じ文字列を得る。
pub fn format_float(f: f64) -> String {
    format!("{}", f)
}

/// Float を deterministic binary encoding 用の bit 列へ変換する（§4.5）。
///
/// NaN は [`CANONICAL_NAN_BITS`] へ畳み込み、それ以外は `f.to_bits()` で符号ビットを保持する
/// （`-0.0` と `+0.0` は別の bit になる）。値としての等価（`-0.0 == 0.0` は `true`）とは別層で、
/// serialize / hash が backend 間で揺れないようにするためのもの。
pub fn float_to_deterministic_bits(f: f64) -> u64 {
    if f.is_nan() {
        CANONICAL_NAN_BITS
    } else {
        f.to_bits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_folds_all_nan_sources_to_canonical_bits() {
        let nan_sources = [
            f64::NAN,
            0.0f64 / 0.0f64,
            f64::INFINITY - f64::INFINITY,
            f64::from_bits(0x7ff0_0000_0000_0001),
        ];
        for src in nan_sources {
            assert_eq!(canonicalize_float(src).to_bits(), CANONICAL_NAN_BITS);
        }
    }

    #[test]
    fn canonicalize_preserves_signed_zero_but_bits_distinguish_it() {
        // 値としての符号ビットは保持する。
        assert_eq!(canonicalize_float(-0.0).to_bits(), (-0.0f64).to_bits());
        // deterministic bits では -0.0 と +0.0 を区別する。
        assert_ne!(
            float_to_deterministic_bits(-0.0),
            float_to_deterministic_bits(0.0)
        );
        // 一方で値としては等価。
        assert!(-0.0f64 == 0.0f64);
    }

    #[test]
    fn parse_float_is_locale_independent() {
        assert_eq!(parse_float("3.14").unwrap(), 3.14);
        assert_eq!(format_float(3.14), "3.14");
        assert!(parse_float("3,14").is_err());
    }

    #[test]
    fn format_float_matches_std_for_special_values() {
        for f in [f64::INFINITY, f64::NEG_INFINITY, canonical_nan()] {
            assert_eq!(format_float(f), format!("{}", f));
        }
    }
}
