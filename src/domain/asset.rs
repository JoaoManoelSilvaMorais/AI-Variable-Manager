use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::record::Record;

pub const REASON_EXACT_NORMALIZED: &str = "exact_normalized";
pub const REASON_FUZZY_STEM: &str = "fuzzy_stem";
pub const REASON_AMBIGUOUS_TIE: &str = "ambiguous_tie";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Jpg,
    Jpeg,
    Png,
    Tiff,
    Tif,
}

impl ImageFormat {
    pub fn from_extension(ext: &str) -> Option<Self> {
        let normalized = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        match normalized.as_str() {
            "jpg" => Some(Self::Jpg),
            "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            "tiff" => Some(Self::Tiff),
            "tif" => Some(Self::Tif),
            _ => None,
        }
    }

    pub fn canonical_ext(&self) -> &'static str {
        match self {
            Self::Jpg => "jpg",
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Tiff => "tiff",
            Self::Tif => "tif",
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Jpg | Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Tiff | Self::Tif => "image/tiff",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub project_id: String,
    pub source_path: PathBuf,
    pub file_name: String,
    pub stem: String,
    pub normalized_stem: String,
    pub format: ImageFormat,
    pub file_size_bytes: u64,
    pub modified_epoch_ms: i64,
    pub exists_on_disk: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannedAsset {
    pub source_path: PathBuf,
    pub file_name: String,
    pub stem: String,
    pub normalized_stem: String,
    pub format: ImageFormat,
    pub file_size_bytes: u64,
    pub modified_epoch_ms: i64,
}

impl ScannedAsset {
    pub fn into_asset(self, id: String, project_id: String) -> Asset {
        Asset {
            id,
            project_id,
            source_path: self.source_path,
            file_name: self.file_name,
            stem: self.stem,
            normalized_stem: self.normalized_stem,
            format: self.format,
            file_size_bytes: self.file_size_bytes,
            modified_epoch_ms: self.modified_epoch_ms,
            exists_on_disk: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResult {
    pub root_dir: PathBuf,
    pub assets: Vec<ScannedAsset>,
    pub skipped_unsupported: usize,
    pub skipped_by_extension: BTreeMap<String, usize>,
    pub unreadable_entries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchCandidate {
    pub record_id: String,
    pub variable_id: String,
    pub asset_id: String,
    pub score: f64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateAssetAssignment {
    pub asset_id: String,
    pub record_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PhotoDiagnostics {
    pub total_records: usize,
    pub total_scanned_assets: usize,
    pub matched_count: usize,
    pub suggested_count: usize,
    pub ambiguous_count: usize,
    pub records_missing_photo: Vec<String>,
    pub unmatched_asset_ids: Vec<String>,
    pub duplicate_asset_assignments: Vec<DuplicateAssetAssignment>,
    pub missing_on_disk_asset_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmatchedPool {
    pub records: Vec<Record>,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchLinkResult {
    pub linked_count: usize,
    pub duplicate_asset_warnings: Vec<DuplicateAssetAssignment>,
}

#[cfg(test)]
mod tests {
    use super::ImageFormat;

    #[test]
    fn image_format_from_extension_accepts_supported_and_rejects_others() {
        assert_eq!(ImageFormat::from_extension("JPG"), Some(ImageFormat::Jpg));
        assert_eq!(
            ImageFormat::from_extension(".jpeg"),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(ImageFormat::from_extension("Png"), Some(ImageFormat::Png));
        assert_eq!(ImageFormat::from_extension("TIFF"), Some(ImageFormat::Tiff));
        assert_eq!(ImageFormat::from_extension("tif"), Some(ImageFormat::Tif));

        assert_eq!(ImageFormat::from_extension("heic"), None);
        assert_eq!(ImageFormat::from_extension("pdf"), None);
        assert_eq!(ImageFormat::from_extension("webp"), None);
        assert_eq!(ImageFormat::from_extension(""), None);
    }
}
