use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::project::{PathStrategy, ProjectMode};
use crate::domain::variable::VariableKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateVariableTrait {
    TextContent,
    Fileref,
    Visibility,
    GraphData,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateVariable {
    pub var_name: String,
    pub trait_kind: TemplateVariableTrait,
    pub category: Option<String>,
    pub sample_uses_p_wrapper: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedXmlTemplate {
    pub var_set_name: Option<String>,
    pub variables: Vec<TemplateVariable>,
    pub entity_map: HashMap<String, String>,
    pub sample_datasets_count: usize,
    pub sample_datasets_inner_span: (usize, usize),
    pub sample_dataset_prefix: String,
    pub has_c2pa_metadata: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XmlColumnBinding {
    pub xml_tag_name: String,
    pub kind: VariableKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XmlCellValue {
    Text(String),
    LinkedFile(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XmlDatasetEntry {
    pub dataset_name: String,
    pub values: Vec<XmlCellValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XmlExportSpec {
    pub mode: ProjectMode,
    pub path_strategy: PathStrategy,
    pub template_raw: Option<String>,
    pub var_set_name: String,
    pub columns: Vec<XmlColumnBinding>,
    pub datasets: Vec<XmlDatasetEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefixUpdateReport {
    pub export_dir: PathBuf,
    pub updated_references: usize,
    pub verified_assets: usize,
    pub missing_assets: Vec<PathBuf>,
    pub updated_dados_xml: PathBuf,
    pub updated_manifest_json: PathBuf,
}
