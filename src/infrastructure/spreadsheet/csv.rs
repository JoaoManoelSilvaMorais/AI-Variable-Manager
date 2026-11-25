use std::collections::HashMap;
use std::path::Path;

use crate::domain::ports::RawTable;
use crate::shared::error::{AppError, AppResult};

const CANDIDATE_DELIMITERS: &[char] = &[';', ',', '\t'];

/// Reads a `.csv` file strictly read-only, stripping UTF-8 BOM, falling back to Windows-1252,
/// and auto-detecting `;`, `,`, or `\t` delimiters.
pub fn read_csv_table(path: &Path, max_rows: Option<usize>) -> AppResult<RawTable> {
    let raw_bytes = std::fs::read(path).map_err(|e| AppError::io("ler arquivo CSV", e))?;
    if raw_bytes.is_empty() {
        return Ok(RawTable {
            sheet_name: None,
            rows: Vec::new(),
            detected_delimiter: None,
            detected_encoding: Some("UTF-8".to_string()),
            total_rows_in_source: 0,
            precision_warnings: Vec::new(),
        });
    }

    let (text, detected_encoding, mut precision_warnings) = decode_csv_bytes(&raw_bytes);
    let delimiter = detect_csv_delimiter(&text);

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter as u8)
        .from_reader(text.as_bytes());

    let mut rows = Vec::new();
    for (idx, record_res) in reader.records().enumerate() {
        let record = record_res.map_err(|e| {
            AppError::spreadsheet(format!("Falha ao interpretar linha {} do CSV: {e}", idx + 1))
        })?;
        rows.push(record.iter().map(str::to_string).collect());
    }

    if detected_encoding.contains("lossy") {
        precision_warnings.push(
            "O arquivo continha bytes inválidos em Windows-1252 que foram substituídos por ''."
                .to_string(),
        );
    }

    let total_rows_in_source = rows.len();
    if let Some(limit) = max_rows {
        rows.truncate(limit);
    }

    Ok(RawTable {
        sheet_name: None,
        rows,
        detected_delimiter: Some(delimiter),
        detected_encoding: Some(detected_encoding),
        total_rows_in_source,
        precision_warnings,
    })
}

fn decode_csv_bytes(bytes: &[u8]) -> (String, String, Vec<String>) {
    if let Some(without_bom) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        if let Ok(utf8) = std::str::from_utf8(without_bom) {
            return (utf8.to_string(), "UTF-8 (BOM)".to_string(), Vec::new());
        }
        let (decoded, had_undef) = decode_windows_1252(without_bom);
        let label = if had_undef {
            "Windows-1252 (lossy)".to_string()
        } else {
            "Windows-1252".to_string()
        };
        return (decoded, label, Vec::new());
    }

    if let Ok(utf8) = std::str::from_utf8(bytes) {
        return (utf8.to_string(), "UTF-8".to_string(), Vec::new());
    }

    let (decoded, had_undef) = decode_windows_1252(bytes);
    let label = if had_undef {
        "Windows-1252 (lossy)".to_string()
    } else {
        "Windows-1252".to_string()
    };
    (decoded, label, Vec::new())
}

/// Decodes Windows-1252 (cp1252) bytes to UTF-8, replacing undefined `0x81/0x8D/0x8F/0x90/0x9D`
/// with `U+FFFD` without panicking.
pub fn decode_windows_1252(bytes: &[u8]) -> (String, bool) {
    let mut out = String::with_capacity(bytes.len());
    let mut had_undef = false;
    for &b in bytes {
        let ch = match b {
            0x00..=0x7F => char::from(b),
            0x80 => '\u{20AC}',
            0x82 => '\u{201A}',
            0x83 => '\u{0192}',
            0x84 => '\u{201E}',
            0x85 => '\u{2026}',
            0x86 => '\u{2020}',
            0x87 => '\u{2021}',
            0x88 => '\u{02C6}',
            0x89 => '\u{2030}',
            0x8A => '\u{0160}',
            0x8B => '\u{2039}',
            0x8C => '\u{0152}',
            0x8E => '\u{017D}',
            0x91 => '\u{2018}',
            0x92 => '\u{2019}',
            0x93 => '\u{201C}',
            0x94 => '\u{201D}',
            0x95 => '\u{2022}',
            0x96 => '\u{2013}',
            0x97 => '\u{2014}',
            0x98 => '\u{02DC}',
            0x99 => '\u{2122}',
            0x9A => '\u{0161}',
            0x9B => '\u{203A}',
            0x9C => '\u{0153}',
            0x9E => '\u{017E}',
            0x9F => '\u{0178}',
            0x81 | 0x8D | 0x8F | 0x90 | 0x9D => {
                had_undef = true;
                '\u{FFFD}'
            }
            0xA0..=0xFF => char::from(b),
        };
        out.push(ch);
    }
    (out, had_undef)
}

/// Auto-detects the most consistent delimiter among `;`, `,`, and `\t`.
pub fn detect_csv_delimiter(text: &str) -> char {
    let mut best_delim = ',';
    let mut best_score = (0usize, 0usize, 0usize);

    for &delim in CANDIDATE_DELIMITERS {
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .delimiter(delim as u8)
            .from_reader(text.as_bytes());

        let mut width_counts: HashMap<usize, usize> = HashMap::new();
        let mut multi_col_rows = 0usize;
        let mut total_cols_sum = 0usize;

        for rec_res in rdr.records().take(16) {
            let Ok(rec) = rec_res else {
                continue;
            };
            let len = rec.len();
            if len > 1 {
                multi_col_rows += 1;
                total_cols_sum += len;
            }
            *width_counts.entry(len).or_insert(0) += 1;
        }

        let modal_multi = width_counts
            .into_iter()
            .filter(|(w, _)| *w > 1)
            .map(|(_, cnt)| cnt)
            .max()
            .unwrap_or(0);

        let score = (modal_multi, multi_col_rows, total_cols_sum);
        if score > best_score {
            best_score = score;
            best_delim = delim;
        }
    }

    best_delim
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_windows_1252_handles_accents_endash_and_undefined_bytes() {
        let raw = b"Jo\xE3o \x96 Designer \x81\x8D\xFF";
        let (decoded, had_undef) = decode_windows_1252(raw);
        assert!(had_undef);
        assert_eq!(decoded, "João \u{2013} Designer \u{FFFD}\u{FFFD}ÿ");
    }

    #[test]
    fn detect_csv_delimiter_picks_semicolon_even_with_title_banner() {
        let sample = "RELATÓRIO DE CRACHÁS\r\nmatricula;nome;funcao;foto\r\n001;Ana;Designer;ana.jpg\r\n";
        assert_eq!(detect_csv_delimiter(sample), ';');
    }
}
