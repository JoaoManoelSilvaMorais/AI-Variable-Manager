use crate::shared::error::{AppError, AppResult};

/// Parses tabular clipboard text (TSV from Excel/Sheets or CSV) into rows of strings,
/// preserving leading zeros, whitespace inside cells, and quoted multiline fields.
pub fn parse_clipboard_block(raw: &str) -> AppResult<Vec<Vec<String>>> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }

    let delim = if raw.contains('\t') {
        b'\t'
    } else if raw.contains(',') {
        b','
    } else if raw.contains(';') {
        b';'
    } else {
        b'\t'
    };

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delim)
        .from_reader(raw.as_bytes());

    let mut rows = Vec::new();
    for (idx, rec_res) in reader.records().enumerate() {
        let rec = rec_res.map_err(|e| {
            AppError::validation(format!(
                "Conteúdo colado inválido na linha {}: {e}",
                idx + 1
            ))
        })?;
        rows.push(rec.iter().map(str::to_string).collect());
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clipboard_block_handles_empty_tsv_and_quoted_csv() {
        assert!(parse_clipboard_block("  \r\n ").unwrap().is_empty());

        let tsv = parse_clipboard_block("0001\tJosé — & < >\r\n0002\tAna\r\n").unwrap();
        assert_eq!(
            tsv,
            vec![
                vec!["0001", "José — & < >"],
                vec!["0002", "Ana"],
            ]
        );

        let csv = parse_clipboard_block("\"Silva, João\",\"Linha 1\nLinha 2\"\r\n").unwrap();
        assert_eq!(csv, vec![vec!["Silva, João", "Linha 1\nLinha 2"]]);
    }
}
