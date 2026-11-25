use std::path::Path;

use crate::domain::ports::{RawTable, SpreadsheetReader};
use crate::infrastructure::spreadsheet::csv::read_csv_table;
use crate::infrastructure::spreadsheet::xlsx::{list_xlsx_sheets, read_xlsx_table};
use crate::shared::error::{AppError, AppResult};

#[derive(Debug, Default, Clone, Copy)]
pub struct CompositeSpreadsheetReader;

impl CompositeSpreadsheetReader {
    pub fn new() -> Self {
        Self
    }

    fn extension_lower(path: &Path) -> String {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default()
    }
}

impl SpreadsheetReader for CompositeSpreadsheetReader {
    fn list_sheets(&self, path: &Path) -> AppResult<Vec<String>> {
        match Self::extension_lower(path).as_str() {
            "xlsx" | "xlsm" => list_xlsx_sheets(path),
            "csv" | "tsv" | "txt" => {
                if !path.exists() {
                    return Err(AppError::io(
                        "abrir planilha CSV",
                        std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            format!("Arquivo não encontrado: {}", path.display()),
                        ),
                    ));
                }
                Ok(vec!["CSV".to_string()])
            }
            other => Err(AppError::unsupported_format(format!(
                "Formato de planilha não suportado: '.{other}'. Utilize arquivos .xlsx ou .csv."
            ))),
        }
    }

    fn read_table(
        &self,
        path: &Path,
        sheet: Option<&str>,
        max_rows: Option<usize>,
    ) -> AppResult<RawTable> {
        match Self::extension_lower(path).as_str() {
            "xlsx" | "xlsm" => read_xlsx_table(path, sheet, max_rows),
            "csv" | "tsv" | "txt" => read_csv_table(path, max_rows),
            other => Err(AppError::unsupported_format(format!(
                "Formato de planilha não suportado: '.{other}'. Utilize arquivos .xlsx ou .csv."
            ))),
        }
    }
}
