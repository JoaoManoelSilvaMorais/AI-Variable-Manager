use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use crate::domain::asset::{Asset, ImageFormat};
use crate::domain::ports::{PackageExporter, ProjectRepository, XmlEngine};
use crate::domain::project::{ProjectMode, ProjectSnapshot};
use crate::domain::record::{MatchStatus, RecordValue};
use crate::domain::validation::{
    AuditReport, ValidationCode, ValidationIssue, ValidationSeverity, ValidationTarget,
};
use crate::domain::variable::VariableKind;
use crate::domain::xml_model::TemplateVariableTrait;
use crate::infrastructure::export::AtomicPackageExporter;
use crate::infrastructure::xml::IllustratorXmlEngine;
use crate::shared::error::AppResult;
use crate::shared::text_norm::is_valid_xml_ncname;

pub struct ValidationService {
    repo: Arc<dyn ProjectRepository>,
    xml_engine: IllustratorXmlEngine,
    exporter: AtomicPackageExporter,
}

impl ValidationService {
    pub fn new(repo: Arc<dyn ProjectRepository>) -> Self {
        Self {
            repo,
            xml_engine: IllustratorXmlEngine::new(),
            exporter: AtomicPackageExporter::new(),
        }
    }

    pub fn audit_project(&self, project_id: &str) -> AppResult<AuditReport> {
        self.audit_project_with_target(project_id, None)
    }

    pub fn audit_project_with_target(
        &self,
        project_id: &str,
        target_dir: Option<&Path>,
    ) -> AppResult<AuditReport> {
        let snap = self.repo.open_project(project_id)?;
        let mut issues = Vec::new();

        self.check_project_shape(&snap, &mut issues);
        self.check_xml_names_and_datasets(&snap, &mut issues);
        self.check_template_compatibility(&snap, &mut issues);
        self.check_cells_and_assets(&snap, &mut issues);

        if let Some(dir) = target_dir {
            let existing = self.exporter.check_existing_targets(dir)?;
            if !existing.is_empty() {
                issues.push(ValidationIssue {
                    code: ValidationCode::W06TargetFilesExist,
                    severity: ValidationSeverity::Warning,
                    title: "Pasta de destino já contém exportação".to_string(),
                    message: format!(
                        "A pasta '{}' já possui {} arquivo(s)/subpasta(s) de exportação que exigirão confirmação de sobrescrita.",
                        dir.display(),
                        existing.len()
                    ),
                    target: ValidationTarget::ExportTarget {
                        path: dir.to_path_buf(),
                    },
                });
            }
        }

        Ok(AuditReport::from_issues(issues))
    }

    fn check_project_shape(&self, snap: &ProjectSnapshot, issues: &mut Vec<ValidationIssue>) {
        if snap.variables.is_empty() || snap.records.is_empty() {
            issues.push(ValidationIssue {
                code: ValidationCode::E01EmptyProject,
                severity: ValidationSeverity::BlockingError,
                title: "Projeto sem dados suficientes para exportação".to_string(),
                message: "Cadastre pelo menos 1 variável (coluna) e 1 registro antes de exportar."
                    .to_string(),
                target: ValidationTarget::Project,
            });
        }
    }

    fn check_xml_names_and_datasets(
        &self,
        snap: &ProjectSnapshot,
        issues: &mut Vec<ValidationIssue>,
    ) {
        let mut xml_name_counts: HashMap<&str, usize> = HashMap::new();
        for var in &snap.variables {
            *xml_name_counts.entry(var.xml_name.as_str()).or_insert(0) += 1;
        }
        for var in &snap.variables {
            let invalid = !is_valid_xml_ncname(&var.xml_name);
            let dup = xml_name_counts.get(var.xml_name.as_str()).copied().unwrap_or(0) > 1;
            if invalid || dup {
                issues.push(ValidationIssue {
                    code: ValidationCode::E02InvalidOrDuplicateXmlName,
                    severity: ValidationSeverity::BlockingError,
                    title: "Identificador XML inválido ou duplicado".to_string(),
                    message: format!(
                        "A coluna '{}' possui xml_name '{}' inválido ou duplicado.",
                        var.display_name, var.xml_name
                    ),
                    target: ValidationTarget::Variable {
                        variable_id: var.id.clone(),
                    },
                });
            }
        }

        let mut ds_counts: HashMap<&str, usize> = HashMap::new();
        for rec in &snap.records {
            *ds_counts.entry(rec.dataset_name.trim()).or_insert(0) += 1;
        }
        for rec in &snap.records {
            let trimmed = rec.dataset_name.trim();
            if trimmed.is_empty() || ds_counts.get(trimmed).copied().unwrap_or(0) > 1 {
                issues.push(ValidationIssue {
                    code: ValidationCode::E03DuplicateDatasetName,
                    severity: ValidationSeverity::BlockingError,
                    title: "Nome de Data Set duplicado ou vazio".to_string(),
                    message: format!(
                        "O registro {} possui dataSetName '{}' vazio ou duplicado.",
                        rec.sort_order + 1,
                        rec.dataset_name
                    ),
                    target: ValidationTarget::Record {
                        record_id: rec.id.clone(),
                    },
                });
            }
        }
    }

    fn check_template_compatibility(
        &self,
        snap: &ProjectSnapshot,
        issues: &mut Vec<ValidationIssue>,
    ) {
        if snap.project.mode != ProjectMode::WithTemplate {
            return;
        }

        let Some(raw) = snap.project.xml_template_raw.as_deref() else {
            issues.push(ValidationIssue {
                code: ValidationCode::E08MissingOrInvalidXmlTemplate,
                severity: ValidationSeverity::BlockingError,
                title: "XML-base do Illustrator ausente".to_string(),
                message: "Projetos em 'Modo XML com Modelo' exigem um XML-base importado."
                    .to_string(),
                target: ValidationTarget::Project,
            });
            return;
        };

        let parsed = match self.xml_engine.inspect_template(raw) {
            Ok(p) => p,
            Err(err) => {
                issues.push(ValidationIssue {
                    code: ValidationCode::E08MissingOrInvalidXmlTemplate,
                    severity: ValidationSeverity::BlockingError,
                    title: "XML-base do Illustrator inválido".to_string(),
                    message: err.user_message(),
                    target: ValidationTarget::Project,
                });
                return;
            }
        };

        let mut template_vars_by_name: HashMap<&str, _> = HashMap::new();
        for tv in &parsed.variables {
            if matches!(
                tv.trait_kind,
                TemplateVariableTrait::Visibility
                    | TemplateVariableTrait::GraphData
                    | TemplateVariableTrait::Unknown(_)
            ) {
                issues.push(ValidationIssue {
                    code: ValidationCode::W07IgnoredTemplateVariableType,
                    severity: ValidationSeverity::Warning,
                    title: "Variável avançada do Illustrator ignorada no MVP".to_string(),
                    message: format!(
                        "A variável '{}' do XML-base será preservada na definição sem edição de dados no MVP.",
                        tv.var_name
                    ),
                    target: ValidationTarget::Project,
                });
                continue;
            }
            template_vars_by_name.insert(tv.var_name.as_str(), tv);
        }

        let mut mapped_template_targets: HashMap<&str, usize> = HashMap::new();
        for var in &snap.variables {
            let target_name = var
                .mapped_template_var
                .as_deref()
                .unwrap_or(var.xml_name.as_str());
            *mapped_template_targets.entry(target_name).or_insert(0) += 1;

            let Some(tpl_var) = template_vars_by_name.get(target_name) else {
                issues.push(ValidationIssue {
                    code: ValidationCode::E09TemplateMappingConflict,
                    severity: ValidationSeverity::BlockingError,
                    title: "Variável não encontrada no XML-base".to_string(),
                    message: format!(
                        "A coluna '{}' aponta para '{}', que não existe no XML-base do Illustrator.",
                        var.display_name, target_name
                    ),
                    target: ValidationTarget::Variable {
                        variable_id: var.id.clone(),
                    },
                });
                continue;
            };

            let incompatible = match (var.kind, &tpl_var.trait_kind) {
                (VariableKind::Text, TemplateVariableTrait::Fileref) => true,
                // Allow legacy Adobe templates where image variable uses trait="textcontent" without <p>.
                (VariableKind::LinkedFile, TemplateVariableTrait::TextContent) => {
                    tpl_var.sample_uses_p_wrapper != Some(false)
                }
                _ => false,
            };

            if incompatible {
                issues.push(ValidationIssue {
                    code: ValidationCode::E09TemplateMappingConflict,
                    severity: ValidationSeverity::BlockingError,
                    title: "Incompatibilidade de tipo com o XML-base".to_string(),
                    message: format!(
                        "A coluna '{}' ({}) é incompatível com o tipo declarado para '{}' no XML-base.",
                        var.display_name,
                        var.kind.display_label_pt_br(),
                        tpl_var.var_name
                    ),
                    target: ValidationTarget::Variable {
                        variable_id: var.id.clone(),
                    },
                });
            }
        }

        for (tpl_name, count) in &mapped_template_targets {
            if *count > 1 {
                issues.push(ValidationIssue {
                    code: ValidationCode::E09TemplateMappingConflict,
                    severity: ValidationSeverity::BlockingError,
                    title: "Mapeamento duplicado para variável do XML-base".to_string(),
                    message: format!(
                        "Mais de uma coluna do projeto está mapeada para a variável '{tpl_name}' do XML-base."
                    ),
                    target: ValidationTarget::Project,
                });
            }
        }

        for tpl_name in template_vars_by_name.keys() {
            if !mapped_template_targets.contains_key(tpl_name) {
                issues.push(ValidationIssue {
                    code: ValidationCode::E09TemplateMappingConflict,
                    severity: ValidationSeverity::BlockingError,
                    title: "Variável obrigatória do XML-base sem mapeamento".to_string(),
                    message: format!(
                        "A variável '{tpl_name}' declarada no XML-base não possui nenhuma coluna mapeada no projeto."
                    ),
                    target: ValidationTarget::Project,
                });
            }
        }
    }

    fn check_cells_and_assets(&self, snap: &ProjectSnapshot, issues: &mut Vec<ValidationIssue>) {
        let val_map: HashMap<(&str, &str), &RecordValue> = snap
            .values
            .iter()
            .map(|v| ((v.record_id.as_str(), v.variable_id.as_str()), v))
            .collect();
        let asset_map: HashMap<&str, &Asset> =
            snap.assets.iter().map(|a| (a.id.as_str(), a)).collect();

        let mut linked_asset_records: HashMap<&str, HashSet<&str>> = HashMap::new();
        let mut linked_asset_ids: HashSet<&str> = HashSet::new();

        for var in &snap.variables {
            if var.kind == VariableKind::Text {
                let mut text_counts: HashMap<&str, usize> = HashMap::new();
                for rec in &snap.records {
                    let txt = val_map
                        .get(&(rec.id.as_str(), var.id.as_str()))
                        .map(|v| v.text_value.as_str())
                        .unwrap_or("");
                    if !txt.trim().is_empty() {
                        *text_counts.entry(txt).or_insert(0) += 1;
                    }
                }

                for rec in &snap.records {
                    let txt = val_map
                        .get(&(rec.id.as_str(), var.id.as_str()))
                        .map(|v| v.text_value.as_str())
                        .unwrap_or("");
                    if txt.trim().is_empty() {
                        let (code, severity, title) = if var.required {
                            (
                                ValidationCode::E04RequiredTextEmpty,
                                ValidationSeverity::BlockingError,
                                "Campo de texto obrigatório vazio",
                            )
                        } else {
                            (
                                ValidationCode::W01OptionalCellEmpty,
                                ValidationSeverity::Warning,
                                "Campo de texto opcional vazio",
                            )
                        };
                        issues.push(ValidationIssue {
                            code,
                            severity,
                            title: title.to_string(),
                            message: format!(
                                "Registro '{}' está com '{}' vazio.",
                                rec.dataset_name, var.display_name
                            ),
                            target: ValidationTarget::Cell {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                            },
                        });
                    } else if text_counts.get(txt).copied().unwrap_or(0) > 1 {
                        issues.push(ValidationIssue {
                            code: ValidationCode::W03DuplicateKeyTextValue,
                            severity: ValidationSeverity::Warning,
                            title: "Valor textual repetido na coluna".to_string(),
                            message: format!(
                                "O valor '{}' aparece em múltiplos registros na coluna '{}'.",
                                txt, var.display_name
                            ),
                            target: ValidationTarget::Cell {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                            },
                        });
                    }
                }
            } else {
                for rec in &snap.records {
                    let rv = val_map.get(&(rec.id.as_str(), var.id.as_str())).copied();
                    let asset_id = rv.and_then(|v| v.asset_id.as_deref());
                    let status = rv.map(|v| v.match_status).unwrap_or(MatchStatus::Unmatched);

                    if let Some(aid) = asset_id {
                        linked_asset_ids.insert(aid);
                        linked_asset_records
                            .entry(aid)
                            .or_default()
                            .insert(rec.id.as_str());

                        match asset_map.get(aid) {
                            None => {
                                issues.push(ValidationIssue {
                                    code: ValidationCode::E06LinkedAssetMissingOnDisk,
                                    severity: ValidationSeverity::BlockingError,
                                    title: "Fotografia vinculada não encontrada".to_string(),
                                    message: format!(
                                        "O ativo '{aid}' referenciado em '{}' não está registrado.",
                                        rec.dataset_name
                                    ),
                                    target: ValidationTarget::Cell {
                                        record_id: rec.id.clone(),
                                        variable_id: var.id.clone(),
                                    },
                                });
                            }
                            Some(asset) => {
                                let ext = asset
                                    .source_path
                                    .extension()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or_default();
                                if ImageFormat::from_extension(ext).is_none() {
                                    issues.push(ValidationIssue {
                                        code: ValidationCode::E07UnsupportedAssetFormat,
                                        severity: ValidationSeverity::BlockingError,
                                        title: "Formato de fotografia não suportado".to_string(),
                                        message: format!(
                                            "Arquivo '{}' possui extensão não suportada para exportação.",
                                            asset.file_name
                                        ),
                                        target: ValidationTarget::Cell {
                                            record_id: rec.id.clone(),
                                            variable_id: var.id.clone(),
                                        },
                                    });
                                } else if !asset.source_path.is_file() {
                                    issues.push(ValidationIssue {
                                        code: ValidationCode::E06LinkedAssetMissingOnDisk,
                                        severity: ValidationSeverity::BlockingError,
                                        title: "Fotografia ausente no disco".to_string(),
                                        message: format!(
                                            "O arquivo '{}' vinculado ao registro '{}' não existe mais no disco.",
                                            asset.source_path.display(),
                                            rec.dataset_name
                                        ),
                                        target: ValidationTarget::Cell {
                                            record_id: rec.id.clone(),
                                            variable_id: var.id.clone(),
                                        },
                                    });
                                }
                            }
                        }
                    } else if status == MatchStatus::Ambiguous {
                        issues.push(ValidationIssue {
                            code: ValidationCode::E10UnresolvedAmbiguousPhoto,
                            severity: ValidationSeverity::BlockingError,
                            title: "Ambiguidade de fotografia não resolvida".to_string(),
                            message: format!(
                                "Registro '{}' possui múltiplas fotos candidatas em '{}' aguardando escolha manual.",
                                rec.dataset_name, var.display_name
                            ),
                            target: ValidationTarget::Cell {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                            },
                        });
                    } else if status == MatchStatus::AutoSuggested {
                        issues.push(ValidationIssue {
                            code: ValidationCode::W02SuggestedPhotoPending,
                            severity: ValidationSeverity::Warning,
                            title: "Sugestão de fotografia pendente de confirmação".to_string(),
                            message: format!(
                                "Registro '{}' possui uma sugestão automática em '{}' não confirmada.",
                                rec.dataset_name, var.display_name
                            ),
                            target: ValidationTarget::Cell {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                            },
                        });
                        if var.required {
                            issues.push(ValidationIssue {
                                code: ValidationCode::E05RequiredPhotoMissing,
                                severity: ValidationSeverity::BlockingError,
                                title: "Fotografia obrigatória não confirmada".to_string(),
                                message: format!(
                                    "Coluna obrigatória '{}' sem fotografia confirmada para '{}'.",
                                    var.display_name, rec.dataset_name
                                ),
                                target: ValidationTarget::Cell {
                                    record_id: rec.id.clone(),
                                    variable_id: var.id.clone(),
                                },
                            });
                        }
                    } else if var.required {
                        issues.push(ValidationIssue {
                            code: ValidationCode::E05RequiredPhotoMissing,
                            severity: ValidationSeverity::BlockingError,
                            title: "Fotografia obrigatória sem vínculo".to_string(),
                            message: format!(
                                "Registro '{}' não possui fotografia vinculada na coluna obrigatória '{}'.",
                                rec.dataset_name, var.display_name
                            ),
                            target: ValidationTarget::Cell {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                            },
                        });
                    } else {
                        issues.push(ValidationIssue {
                            code: ValidationCode::W01OptionalCellEmpty,
                            severity: ValidationSeverity::Warning,
                            title: "Fotografia opcional sem vínculo".to_string(),
                            message: format!(
                                "Registro '{}' está sem fotografia vinculada em '{}'.",
                                rec.dataset_name, var.display_name
                            ),
                            target: ValidationTarget::Cell {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                            },
                        });
                    }
                }
            }
        }

        for (asset_id, rec_ids) in &linked_asset_records {
            if rec_ids.len() > 1 {
                issues.push(ValidationIssue {
                    code: ValidationCode::W04SharedAssetAcrossRecords,
                    severity: ValidationSeverity::Warning,
                    title: "Fotografia compartilhada entre múltiplos registros".to_string(),
                    message: format!(
                        "A mesma fotografia ({asset_id}) está vinculada a {} registros.",
                        rec_ids.len()
                    ),
                    target: ValidationTarget::Asset {
                        asset_id: (*asset_id).to_string(),
                    },
                });
            }
        }

        let unmatched_count = snap
            .assets
            .iter()
            .filter(|a| !linked_asset_ids.contains(a.id.as_str()))
            .count();
        if unmatched_count > 0 {
            issues.push(ValidationIssue {
                code: ValidationCode::W05UnmatchedScannedPhotos,
                severity: ValidationSeverity::Warning,
                title: "Fotografias escaneadas sem uso".to_string(),
                message: format!(
                    "Existem {unmatched_count} fotografia(s) na pasta escaneada que não serão copiadas por não estarem vinculadas a nenhum registro."
                ),
                target: ValidationTarget::Project,
            });
        }
    }
}
