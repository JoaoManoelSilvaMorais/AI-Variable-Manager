use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::asset::{Asset, MatchCandidate};
use crate::domain::record::{Record, RecordValue};
use crate::domain::variable::Variable;
use crate::shared::error::{AppError, AppResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMode {
    WithTemplate,
    WithoutTemplate,
}

impl ProjectMode {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::WithTemplate => "with_template",
            Self::WithoutTemplate => "without_template",
        }
    }

    pub fn from_db_str(raw: &str) -> AppResult<Self> {
        match raw {
            "with_template" => Ok(Self::WithTemplate),
            "without_template" => Ok(Self::WithoutTemplate),
            other => Err(AppError::validation(format!(
                "Modo de projeto inválido: '{other}'"
            ))),
        }
    }

    pub fn display_label_pt_br(&self) -> &'static str {
        match self {
            Self::WithTemplate => "Modo XML com Modelo",
            Self::WithoutTemplate => "Modo Dados sem Modelo",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathStrategy {
    #[default]
    #[serde(alias = "absolute_file_uri", alias = "relative_to_xml")]
    AbsolutePath,
}

impl PathStrategy {
    /// Kept compatible with the existing SQLite `CHECK` constraint on `projects.path_strategy`.
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::AbsolutePath => "absolute_file_uri",
        }
    }

    pub fn from_db_str(raw: &str) -> AppResult<Self> {
        match raw {
            "absolute_path" | "absolute_file_uri" | "relative_to_xml" => Ok(Self::AbsolutePath),
            other => Err(AppError::validation(format!(
                "Estratégia de caminho inválida: '{other}'"
            ))),
        }
    }

    pub fn display_label_pt_br(&self) -> &'static str {
        match self {
            Self::AbsolutePath => "Caminho Absoluto (Windows)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    Light,
    Dark,
    #[default]
    System,
}

impl ThemePreference {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    pub fn from_db_str(raw: &str) -> AppResult<Self> {
        match raw {
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            "system" => Ok(Self::System),
            other => Err(AppError::validation(format!(
                "Preferência de tema inválida: '{other}'"
            ))),
        }
    }

    pub fn display_label_pt_br(&self) -> &'static str {
        match self {
            Self::Light => "Claro",
            Self::Dark => "Escuro",
            Self::System => "Seguir Sistema",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub mode: ProjectMode,
    pub path_strategy: PathStrategy,
    pub xml_template_path: Option<PathBuf>,
    pub xml_template_raw: Option<String>,
    pub photo_root_dir: Option<PathBuf>,
    pub last_export_dir: Option<PathBuf>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub mode: ProjectMode,
    pub path_strategy: PathStrategy,
    pub variable_count: usize,
    pub record_count: usize,
    pub asset_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub project: Project,
    pub variables: Vec<Variable>,
    pub records: Vec<Record>,
    pub values: Vec<RecordValue>,
    pub assets: Vec<Asset>,
    pub match_candidates: Vec<MatchCandidate>,
}

impl ProjectSnapshot {
    pub fn value_for(&self, record_id: &str, variable_id: &str) -> Option<&RecordValue> {
        self.values
            .iter()
            .find(|v| v.record_id == record_id && v.variable_id == variable_id)
    }

    pub fn variable_by_id(&self, variable_id: &str) -> Option<&Variable> {
        self.variables.iter().find(|v| v.id == variable_id)
    }

    pub fn asset_by_id(&self, asset_id: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.id == asset_id)
    }
}

pub fn validate_project_name(name: &str) -> AppResult<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AppError::validation(
            "O nome do projeto não pode estar vazio.",
        ));
    }
    if trimmed.chars().count() > 120 {
        return Err(AppError::validation(
            "O nome do projeto não pode exceder 120 caracteres.",
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_project_name_trims_and_enforces_bounds() {
        assert_eq!(
            validate_project_name("  Crachás SECOM  ").unwrap(),
            "Crachás SECOM"
        );
        assert!(validate_project_name("   ").is_err());

        let exact_120_ascii = "a".repeat(120);
        assert_eq!(validate_project_name(&exact_120_ascii).unwrap().chars().count(), 120);

        let exact_120_unicode = "á".repeat(120);
        assert_eq!(
            validate_project_name(&format!("  {exact_120_unicode}  ")).unwrap(),
            exact_120_unicode
        );

        let too_long_121 = "á".repeat(121);
        assert!(validate_project_name(&too_long_121).is_err());

        let too_long_255 = "a".repeat(255);
        assert!(validate_project_name(&too_long_255).is_err());
    }

    #[test]
    fn enum_db_roundtrips_succeed() {
        for mode in [ProjectMode::WithTemplate, ProjectMode::WithoutTemplate] {
            assert_eq!(ProjectMode::from_db_str(mode.as_db_str()).unwrap(), mode);
        }
        assert_eq!(
            PathStrategy::from_db_str(PathStrategy::AbsolutePath.as_db_str()).unwrap(),
            PathStrategy::AbsolutePath
        );
        assert_eq!(
            PathStrategy::from_db_str("relative_to_xml").unwrap(),
            PathStrategy::AbsolutePath
        );
        assert_eq!(
            PathStrategy::from_db_str("absolute_path").unwrap(),
            PathStrategy::AbsolutePath
        );
        for theme in [
            ThemePreference::Light,
            ThemePreference::Dark,
            ThemePreference::System,
        ] {
            assert_eq!(
                ThemePreference::from_db_str(theme.as_db_str()).unwrap(),
                theme
            );
        }
    }
}
