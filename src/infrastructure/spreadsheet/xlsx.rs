use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use calamine::{Data, Range, Reader, Xlsx, open_workbook};

use crate::domain::ports::RawTable;
use crate::shared::error::{AppError, AppResult};

fn open_xlsx(path: &Path) -> AppResult<Xlsx<BufReader<File>>> {
    if !path.exists() {
        return Err(AppError::io(
            "abrir planilha Excel",
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Arquivo não encontrado: {}", path.display()),
            ),
        ));
    }
    open_workbook::<Xlsx<_>, _>(path)
        .map_err(|e| AppError::spreadsheet(format!("Arquivo .xlsx inválido ou corrompido: {e}")))
}

/// Lists all worksheet names in an `.xlsx` workbook without mutating the source file.
pub fn list_xlsx_sheets(path: &Path) -> AppResult<Vec<String>> {
    let workbook = open_xlsx(path)?;
    Ok(workbook.sheet_names().to_vec())
}

/// Reads an `.xlsx` worksheet into a `RawTable`, preserving leading zeros in text cells.
pub fn read_xlsx_table(
    path: &Path,
    sheet: Option<&str>,
    max_rows: Option<usize>,
) -> AppResult<RawTable> {
    let mut workbook = open_xlsx(path)?;
    let sheet_names = workbook.sheet_names().to_vec();
    if sheet_names.is_empty() {
        return Err(AppError::spreadsheet(
            "A pasta de trabalho Excel não contém abas.",
        ));
    }

    let target_sheet = match sheet {
        Some(name) => {
            if !sheet_names.iter().any(|s| s == name) {
                return Err(AppError::spreadsheet(format!(
                    "Aba '{name}' não encontrada na planilha."
                )));
            }
            name.to_string()
        }
        None => sheet_names[0].clone(),
    };

    let range = workbook.worksheet_range(&target_sheet).map_err(|e| {
        AppError::spreadsheet(format!("Falha ao ler a aba '{target_sheet}': {e}"))
    })?;

    let mut precision_warnings = Vec::new();
    let mut rows = range_to_rows(&range, &mut precision_warnings);
    let total_rows_in_source = rows.len();

    if let Some(limit) = max_rows {
        rows.truncate(limit);
    }

    Ok(RawTable {
        sheet_name: Some(target_sheet),
        rows,
        detected_delimiter: None,
        detected_encoding: Some("OpenXML (UTF-8)".to_string()),
        total_rows_in_source,
        precision_warnings,
    })
}

fn range_to_rows(range: &Range<Data>, warnings: &mut Vec<String>) -> Vec<Vec<String>> {
    let (height, width) = range.get_size();
    if height == 0 || width == 0 {
        return Vec::new();
    }

    let (start_row, start_col) = range.start().unwrap_or((0, 0));
    let total_cols = start_col as usize + width;
    let mut rows = Vec::with_capacity(start_row as usize + height);

    for _ in 0..start_row {
        rows.push(vec![String::new(); total_cols]);
    }

    for (r_offset, row_cells) in range.rows().enumerate() {
        let mut out_row = Vec::with_capacity(total_cols);
        for _ in 0..start_col {
            out_row.push(String::new());
        }
        for (c_offset, cell) in row_cells.iter().enumerate() {
            let formatted = format_xlsx_cell(
                cell,
                start_row as usize + r_offset,
                start_col as usize + c_offset,
                warnings,
            );
            out_row.push(formatted);
        }
        rows.push(out_row);
    }

    rows
}

fn format_xlsx_cell(
    cell: &Data,
    row_idx: usize,
    col_idx: usize,
    warnings: &mut Vec<String>,
) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => format_xlsx_float(*f, row_idx, col_idx, warnings),
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Data::DateTime(dt) => dt.to_string(),
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => e.to_string(),
    }
}

fn format_xlsx_float(f: f64, row_idx: usize, col_idx: usize, warnings: &mut Vec<String>) -> String {
    if !f.is_finite() {
        return String::new();
    }
    if f == 0.0 {
        return "0".to_string();
    }
    if f.fract() == 0.0 && f >= (i64::MIN as f64) && f <= (i64::MAX as f64) {
        if f.abs() >= 1_000_000_000_000_000.0 {
            warnings.push(format!(
                "Linha {}, coluna {}: número grande ({f}) pode ter perdido precisão ou zeros à esquerda no Excel.",
                row_idx + 1,
                col_idx + 1
            ));
        }
        return (f as i64).to_string();
    }
    f.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_xlsx_float_strips_dot_zero_and_keeps_fractions() {
        let mut warnings = Vec::new();
        assert_eq!(format_xlsx_float(1000.0, 0, 0, &mut warnings), "1000");
        assert_eq!(format_xlsx_float(0.0, 0, 0, &mut warnings), "0");
        assert_eq!(format_xlsx_float(-0.0, 0, 0, &mut warnings), "0");
        assert_eq!(format_xlsx_float(12.5, 0, 0, &mut warnings), "12.5");
        assert!(warnings.is_empty());
    }
}
