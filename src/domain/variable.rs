use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::shared::error::{AppError, AppResult};
use crate::shared::text_norm::{is_valid_xml_ncname, resolve_unique_xml_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariableKind {
    Text,
    LinkedFile,
}

impl VariableKind {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::LinkedFile => "linked_file",
        }
    }

    pub fn from_db_str(raw: &str) -> AppResult<Self> {
        match raw {
            "text" => Ok(Self::Text),
            "linked_file" => Ok(Self::LinkedFile),
            other => Err(AppError::validation(format!(
                "Tipo de variável inválido: '{other}'"
            ))),
        }
    }

    pub fn display_label_pt_br(&self) -> &'static str {
        match self {
            Self::Text => "Texto",
            Self::LinkedFile => "Fotografia (Vínculo)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variable {
    pub id: String,
    pub project_id: String,
    pub display_name: String,
    pub xml_name: String,
    pub kind: VariableKind,
    pub required: bool,
    pub sort_order: usize,
    pub mapped_template_var: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewVariableSpec {
    pub display_name: String,
    pub xml_name: Option<String>,
    pub kind: VariableKind,
    pub required: bool,
    pub mapped_template_var: Option<String>,
}

pub fn validate_display_name(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::validation(
            "O nome de exibição da coluna não pode estar vazio.",
        ));
    }
    Ok(trimmed.to_string())
}

pub fn validate_xml_name(xml_name: &str) -> AppResult<()> {
    if !is_valid_xml_ncname(xml_name) {
        return Err(AppError::validation(format!(
            "Identificador XML inválido '{xml_name}': deve começar com letra ou '_' e conter apenas caracteres XML NCName válidos (sem espaços ou ':')."
        )));
    }
    Ok(())
}

pub fn ensure_unique_xml_name(
    xml_name: &str,
    existing: &[Variable],
    ignore_id: Option<&str>,
) -> AppResult<()> {
    let conflict = existing
        .iter()
        .any(|v| Some(v.id.as_str()) != ignore_id && v.xml_name == xml_name);
    if conflict {
        return Err(AppError::conflict(format!(
            "O identificador XML '{xml_name}' já está em uso por outra coluna neste projeto."
        )));
    }
    Ok(())
}

pub fn compute_reordered_ids(
    existing: &[Variable],
    ordered_ids: &[String],
) -> AppResult<Vec<String>> {
    if existing.len() != ordered_ids.len() {
        return Err(AppError::validation(
            "A lista de reordenação não corresponde ao número total de colunas do projeto.",
        ));
    }

    let existing_set: HashSet<&str> = existing.iter().map(|v| v.id.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::with_capacity(ordered_ids.len());

    for id in ordered_ids {
        if !existing_set.contains(id.as_str()) {
            return Err(AppError::validation(format!(
                "Coluna desconhecida na reordenação: '{id}'."
            )));
        }
        if !seen.insert(id.as_str()) {
            return Err(AppError::validation(format!(
                "Coluna duplicada na lista de reordenação: '{id}'."
            )));
        }
    }

    Ok(ordered_ids.to_vec())
}

pub fn build_duplicated_variable(
    source: &Variable,
    new_id: String,
    existing: &[Variable],
    sort_order: usize,
) -> Variable {
    let candidate_xml = format!("{}_copia", source.xml_name);
    let unique_xml = resolve_unique_xml_name(
        &candidate_xml,
        existing.iter().map(|v| v.xml_name.as_str()),
    );
    Variable {
        id: new_id,
        project_id: source.project_id.clone(),
        display_name: format!("{} (Cópia)", source.display_name),
        xml_name: unique_xml,
        kind: source.kind,
        required: source.required,
        sort_order,
        mapped_template_var: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::error::{AppError, DomainError};

    fn sample_var(id: &str, xml: &str, sort_order: usize) -> Variable {
        Variable {
            id: id.to_string(),
            project_id: "proj-1".to_string(),
            display_name: xml.to_string(),
            xml_name: xml.to_string(),
            kind: VariableKind::Text,
            required: false,
            sort_order,
            mapped_template_var: None,
        }
    }

    #[test]
    fn compute_reordered_ids_validates_permutations() {
        let vars = vec![
            sample_var("a", "Nome", 0),
            sample_var("b", "Cargo", 1),
            sample_var("c", "Foto", 2),
        ];

        assert_eq!(
            compute_reordered_ids(&vars, &["c".into(), "a".into(), "b".into()]).unwrap(),
            vec!["c", "a", "b"]
        );
        assert!(compute_reordered_ids(&vars, &["a".into(), "a".into(), "b".into()]).is_err());
        assert!(compute_reordered_ids(&vars, &["a".into(), "b".into()]).is_err());
        assert!(compute_reordered_ids(&vars, &["a".into(), "b".into(), "x".into()]).is_err());
    }

    #[test]
    fn ensure_unique_xml_name_allows_self_and_rejects_other() {
        let vars = vec![sample_var("v1", "Nome", 0), sample_var("v2", "Cargo", 1)];

        assert!(ensure_unique_xml_name("Nome", &vars, Some("v1")).is_ok());
        let err = ensure_unique_xml_name("Nome", &vars, Some("v2")).unwrap_err();
        assert!(matches!(err, AppError::Domain(DomainError::Conflict(_))));
    }

    #[test]
    fn build_duplicated_variable_generates_unique_xml_name_and_copy_label() {
        let v1 = sample_var("v1", "Registro", 0);
        let dup = build_duplicated_variable(&v1, "v2".into(), std::slice::from_ref(&v1), 1);
        assert_eq!(dup.display_name, "Registro (Cópia)");
        assert_eq!(dup.xml_name, "Registro_copia");
        assert_eq!(dup.sort_order, 1);
        assert_eq!(dup.mapped_template_var, None);

        let dup2 = build_duplicated_variable(&v1, "v3".into(), &[v1.clone(), dup.clone()], 1);
        assert_eq!(dup2.xml_name, "Registro_copia_2");
    }
}
