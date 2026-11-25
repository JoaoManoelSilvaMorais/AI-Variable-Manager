use std::collections::BTreeMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

use walkdir::WalkDir;

use crate::domain::asset::{ImageFormat, ScanResult, ScannedAsset};
use crate::domain::ports::PhotoScanner;
use crate::shared::error::{AppError, AppResult};
use crate::shared::text_norm::normalize_stem;

#[derive(Debug, Default, Clone, Copy)]
pub struct LocalPhotoScanner;

impl LocalPhotoScanner {
    pub fn new() -> Self {
        Self
    }
}

/// Reads metadata and decodes the image header of a single candidate file without mutating it.
pub fn inspect_single_image_file(path: &Path) -> AppResult<ScannedAsset> {
    if !path.exists() {
        return Err(AppError::io(
            "acessar arquivo de fotografia",
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Arquivo não encontrado: {}", path.display()),
            ),
        ));
    }
    if !path.is_file() {
        return Err(AppError::validation(format!(
            "O caminho informado não é um arquivo válido: {}",
            path.display()
        )));
    }

    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let format = ImageFormat::from_extension(ext).ok_or_else(|| {
        AppError::unsupported_format(format!(
            "Formato de imagem não suportado ('.{ext}'). Use JPG, JPEG, PNG ou TIFF."
        ))
    })?;

    let metadata = std::fs::metadata(path)
        .map_err(|e| AppError::io("ler metadados da fotografia", e))?;
    let file_size_bytes = metadata.len();
    if file_size_bytes == 0 {
        return Err(AppError::validation(format!(
            "O arquivo de imagem está vazio (0 bytes): {}",
            path.display()
        )));
    }

    // Validate image header/container before accepting manual link.
    let reader = image::ImageReader::open(path)
        .map_err(|e| AppError::io("abrir arquivo de fotografia", e))?
        .with_guessed_format()
        .map_err(|e| AppError::media(format!("Não foi possível identificar o formato da imagem: {e}")))?;
    reader
        .into_dimensions()
        .map_err(|e| AppError::media(format!("Arquivo de imagem corrompido ou ilegível: {e}")))?;

    Ok(build_scanned_asset(path, format, &metadata))
}

fn build_scanned_asset(
    path: &Path,
    format: ImageFormat,
    metadata: &std::fs::Metadata,
) -> ScannedAsset {
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let normalized_stem = normalize_stem(&stem);
    let file_size_bytes = metadata.len();
    let modified_epoch_ms = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    ScannedAsset {
        source_path: path.to_path_buf(),
        file_name,
        stem,
        normalized_stem,
        format,
        file_size_bytes,
        modified_epoch_ms,
    }
}

impl PhotoScanner for LocalPhotoScanner {
    fn scan_directory(&self, root: &Path) -> AppResult<ScanResult> {
        if !root.exists() {
            return Err(AppError::io(
                "escanear diretório de fotografias",
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Diretório não encontrado: {}", root.display()),
                ),
            ));
        }
        if !root.is_dir() {
            return Err(AppError::validation(format!(
                "O caminho selecionado não é um diretório: {}",
                root.display()
            )));
        }

        let mut assets = Vec::new();
        let mut skipped_unsupported = 0usize;
        let mut skipped_by_extension: BTreeMap<String, usize> = BTreeMap::new();
        let mut unreadable_entries = Vec::new();

        for entry_res in WalkDir::new(root).follow_links(false) {
            let entry = match entry_res {
                Ok(e) => e,
                Err(err) => {
                    unreadable_entries.push(err.to_string());
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or_default();

            if let Some(format) = ImageFormat::from_extension(ext) {
                match entry.metadata() {
                    Ok(meta) => assets.push(build_scanned_asset(path, format, &meta)),
                    Err(err) => unreadable_entries.push(err.to_string()),
                }
            } else {
                skipped_unsupported += 1;
                let key = if ext.is_empty() {
                    "(sem extensão)".to_string()
                } else {
                    ext.to_ascii_lowercase()
                };
                *skipped_by_extension.entry(key).or_insert(0) += 1;
            }
        }

        assets.sort_by(|a, b| {
            a.file_name
                .cmp(&b.file_name)
                .then_with(|| a.source_path.cmp(&b.source_path))
        });

        Ok(ScanResult {
            root_dir: root.to_path_buf(),
            assets,
            skipped_unsupported,
            skipped_by_extension,
            unreadable_entries,
        })
    }
}
