use std::sync::Arc;

use uuid::Uuid;

use crate::domain::ports::ProjectRepository;
use crate::domain::project::{
    Project, ProjectMode, ProjectSnapshot, ProjectSummary, ThemePreference, validate_project_name,
};
use crate::domain::variable::{
    Variable, VariableKind, ensure_unique_xml_name, validate_display_name, validate_xml_name,
};
use crate::shared::error::{AppError, AppResult, DomainError};
use crate::shared::text_norm::{resolve_unique_xml_name, suggest_xml_name};

const SETTING_THEME: &str = "theme_preference";
const SETTING_LAST_PROJECT: &str = "last_opened_project_id";

pub struct ProjectService {
    repo: Arc<dyn ProjectRepository>,
}

impl ProjectService {
    pub fn new(repo: Arc<dyn ProjectRepository>) -> Self {
        Self { repo }
    }

    pub fn create_project(&self, name: &str, mode: ProjectMode) -> AppResult<Project> {
        let validated_name = validate_project_name(name)?;
        let project = self.repo.create_project(&validated_name, mode)?;
        self.repo
            .set_setting(SETTING_LAST_PROJECT, &project.id)?;
        Ok(project)
    }

    pub fn open_project(&self, project_id: &str) -> AppResult<ProjectSnapshot> {
        let snapshot = self.repo.open_project(project_id)?;
        self.repo.set_setting(SETTING_LAST_PROJECT, project_id)?;
        Ok(snapshot)
    }

    pub fn list_projects(&self) -> AppResult<Vec<ProjectSummary>> {
        self.repo.list_projects()
    }

    pub fn rename_project(&self, project_id: &str, new_name: &str) -> AppResult<()> {
        let validated_name = validate_project_name(new_name)?;
        self.repo.rename_project(project_id, &validated_name)
    }

    pub fn delete_project(&self, project_id: &str) -> AppResult<()> {
        self.repo.delete_project(project_id)
    }

    pub fn update_project_settings(&self, project: &Project) -> AppResult<()> {
        let validated_name = validate_project_name(&project.name)?;
        if validated_name != project.name {
            let mut normalized = project.clone();
            normalized.name = validated_name;
            return self.repo.update_project_settings(&normalized);
        }
        self.repo.update_project_settings(project)
    }

    pub fn create_variable(
        &self,
        project_id: &str,
        display_name: &str,
        xml_name: Option<&str>,
        kind: VariableKind,
        required: bool,
    ) -> AppResult<Variable> {
        const MAX_ATTEMPTS: usize = 128;
        let trimmed_display = validate_display_name(display_name)?;
        let explicit_xml = match xml_name {
            Some(raw) => {
                let trimmed = raw.trim();
                validate_xml_name(trimmed)?;
                Some(trimmed.to_string())
            }
            None => None,
        };

        let var_id = Uuid::new_v4().to_string();
        for _attempt in 0..MAX_ATTEMPTS {
            let snap = self.repo.open_project(project_id)?;

            let final_xml_name = match &explicit_xml {
                Some(trimmed_xml) => {
                    ensure_unique_xml_name(trimmed_xml, &snap.variables, None)?;
                    trimmed_xml.clone()
                }
                None => {
                    let base = suggest_xml_name(&trimmed_display);
                    resolve_unique_xml_name(
                        &base,
                        snap.variables.iter().map(|v| v.xml_name.as_str()),
                    )
                }
            };

            let sort_order = snap
                .variables
                .iter()
                .map(|v| v.sort_order)
                .max()
                .map(|m| {
                    m.checked_add(1).ok_or_else(|| {
                        AppError::validation(
                            "Limite máximo de ordem atingido para variáveis.",
                        )
                    })
                })
                .transpose()?
                .unwrap_or(0);

            let var = Variable {
                id: var_id.clone(),
                project_id: project_id.to_string(),
                display_name: trimmed_display.clone(),
                xml_name: final_xml_name,
                kind,
                required,
                sort_order,
                mapped_template_var: None,
            };

            match self.repo.upsert_variable(&var) {
                Ok(()) => return Ok(var),
                Err(err)
                    if is_sort_order_conflict(&err)
                        || (explicit_xml.is_none() && is_xml_name_conflict(&err)) =>
                {
                    continue;
                }
                Err(err) => return Err(err),
            }
        }

        Err(AppError::conflict(
            "Não foi possível criar a variável após múltiplas tentativas concorrentes.",
        ))
    }

    pub fn rename_variable(
        &self,
        project_id: &str,
        variable_id: &str,
        new_display_name: &str,
        new_xml_name: Option<&str>,
    ) -> AppResult<Variable> {
        let trimmed_display = validate_display_name(new_display_name)?;
        let snap = self.repo.open_project(project_id)?;
        let mut var = snap
            .variables
            .iter()
            .find(|v| v.id == variable_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("Variável", variable_id))?;

        if let Some(raw_xml) = new_xml_name {
            let trimmed_xml = raw_xml.trim();
            validate_xml_name(trimmed_xml)?;
            ensure_unique_xml_name(trimmed_xml, &snap.variables, Some(variable_id))?;
            var.xml_name = trimmed_xml.to_string();
        }

        var.display_name = trimmed_display;
        self.repo.upsert_variable(&var)?;
        Ok(var)
    }

    pub fn update_variable_options(
        &self,
        project_id: &str,
        variable_id: &str,
        kind: VariableKind,
        required: bool,
        mapped_template_var: Option<String>,
    ) -> AppResult<Variable> {
        let snap = self.repo.open_project(project_id)?;
        let mut var = snap
            .variables
            .into_iter()
            .find(|v| v.id == variable_id)
            .ok_or_else(|| AppError::not_found("Variável", variable_id))?;

        var.kind = kind;
        var.required = required;
        var.mapped_template_var = mapped_template_var;
        self.repo.upsert_variable(&var)?;
        Ok(var)
    }

    pub fn duplicate_variable(
        &self,
        project_id: &str,
        variable_id: &str,
    ) -> AppResult<Variable> {
        self.repo.duplicate_variable(project_id, variable_id)
    }

    pub fn delete_variable(&self, project_id: &str, variable_id: &str) -> AppResult<()> {
        self.repo.delete_variable(project_id, variable_id)
    }

    pub fn reorder_variables(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()> {
        self.repo.reorder_variables(project_id, ordered_ids)
    }

    pub fn get_theme_preference(&self) -> AppResult<ThemePreference> {
        match self.repo.get_setting(SETTING_THEME)? {
            Some(raw) => ThemePreference::from_db_str(&raw),
            None => Ok(ThemePreference::System),
        }
    }

    pub fn set_theme_preference(&self, pref: ThemePreference) -> AppResult<()> {
        self.repo.set_setting(SETTING_THEME, pref.as_db_str())
    }

    pub fn get_last_opened_project_id(&self) -> AppResult<Option<String>> {
        self.repo.get_setting(SETTING_LAST_PROJECT)
    }

    pub fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        self.repo.get_setting(key)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.repo.set_setting(key, value)
    }
}

fn is_sort_order_conflict(err: &AppError) -> bool {
    matches!(
        err,
        AppError::Domain(DomainError::Conflict(msg))
            if msg.contains("ordem da coluna")
                || msg.contains("ordem da variável")
                || msg.contains("sort_order")
    )
}

fn is_xml_name_conflict(err: &AppError) -> bool {
    matches!(
        err,
        AppError::Domain(DomainError::Conflict(msg))
            if msg.contains("identificador XML") || msg.contains("xml_name")
    )
}
