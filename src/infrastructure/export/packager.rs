use std::fs::{File, create_dir_all};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tempfile::Builder as TempDirBuilder;

use crate::domain::ports::{ExportPlan, ExportSummary, PackageExporter, XmlEngine};
use crate::domain::project::PathStrategy;
use crate::infrastructure::xml::IllustratorXmlEngine;
use crate::shared::error::{AppError, AppResult, DomainError};
use crate::shared::time::utc_now_rfc3339;

const PACKAGE_FILES: &[&str] = &["dados.xml", "imagens", "manifest.json", "relatorio.csv"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportManifestAsset {
    pub asset_id: String,
    pub source_path: String,
    pub exported_rel_path: String,
    pub exported_uri: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportManifest {
    pub project_id: String,
    pub project_name: String,
    pub path_strategy: PathStrategy,
    pub dataset_count: usize,
    pub export_dir: String,
    pub exported_at: String,
    pub updated_at: String,
    pub asset_mappings: Vec<ExportManifestAsset>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AtomicPackageExporter;

impl AtomicPackageExporter {
    pub fn new() -> Self {
        Self
    }
}

impl PackageExporter for AtomicPackageExporter {
    fn check_existing_targets(&self, target_dir: &Path) -> AppResult<Vec<PathBuf>> {
        if !target_dir.exists() {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for item in PACKAGE_FILES {
            let p = target_dir.join(item);
            if p.exists() {
                found.push(p);
            }
        }
        Ok(found)
    }

    fn export_atomic(
        &self,
        plan: &ExportPlan,
        overwrite_confirmed: bool,
    ) -> AppResult<ExportSummary> {
        // 1. Validate XML bytes cleanly before creating staging or target files.
        let xml_engine = IllustratorXmlEngine::new();
        xml_engine.validate_xml_structure(&plan.xml_content, plan.expected_datasets)?;

        // 2. Guard against overwriting existing target files unless confirmed.
        let existing = self.check_existing_targets(&plan.target_dir)?;
        if !existing.is_empty() && !overwrite_confirmed {
            return Err(AppError::Domain(
                DomainError::OverwriteConfirmationRequired {
                    target_dir: plan.target_dir.to_string_lossy().into_owned(),
                    existing_paths: existing
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect(),
                },
            ));
        }

        // 3. Ensure target_dir is creatable (fails cleanly if parent is a regular file).
        if plan.target_dir.exists() && !plan.target_dir.is_dir() {
            return Err(AppError::export(format!(
                "O destino '{}' já existe como arquivo.",
                plan.target_dir.display()
            )));
        }
        create_dir_all(&plan.target_dir)
            .map_err(|e| AppError::io("criar pasta de exportação", e))?;

        let stage_parent = plan.target_dir.parent().unwrap_or(&plan.target_dir);
        let stage_dir = TempDirBuilder::new()
            .prefix(".aivm_stage_")
            .tempdir_in(stage_parent)
            .map_err(|e| AppError::io("criar diretório temporário de exportação", e))?;

        let stage_path = stage_dir.path();
        let stage_images = stage_path.join("imagens");
        create_dir_all(&stage_images)
            .map_err(|e| AppError::io("criar subpasta temporária imagens/", e))?;

        // 4. Stream-copy linked photos into staging `imagens/asset_XXX.ext` without full-res decoding.
        let mut manifest_assets = Vec::with_capacity(plan.asset_copies.len());
        let mut total_asset_bytes = 0u64;

        for copy_item in &plan.asset_copies {
            if !copy_item.source_path.is_file() {
                return Err(AppError::io(
                    "copiar fotografia vinculada durante exportação",
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!(
                            "Arquivo de fotografia ausente: {}",
                            copy_item.source_path.display()
                        ),
                    ),
                ));
            }

            let dst_rel = copy_item.target_rel_path.replace('\\', "/");
            let file_name = dst_rel
                .rsplit('/')
                .next()
                .ok_or_else(|| AppError::export("Caminho relativo de ativo inválido."))?;
            let dst_file = stage_images.join(file_name);

            let src_file = File::open(&copy_item.source_path)
                .map_err(|e| AppError::io("abrir fotografia original para cópia", e))?;
            let out_file = File::create(&dst_file)
                .map_err(|e| AppError::io("criar cópia da fotografia no pacote", e))?;

            let mut reader = BufReader::new(src_file);
            let mut writer = BufWriter::new(out_file);
            let copied_bytes = std::io::copy(&mut reader, &mut writer)
                .map_err(|e| AppError::io("copiar bytes da fotografia", e))?;
            writer
                .flush()
                .map_err(|e| AppError::io("finalizar escrita da fotografia", e))?;

            total_asset_bytes += copied_bytes;

            let exported_uri = crate::infrastructure::xml::format_asset_target(
                &plan.target_dir,
                &dst_rel,
            );
            manifest_assets.push(ExportManifestAsset {
                asset_id: copy_item.asset_id.clone(),
                source_path: copy_item.source_path.to_string_lossy().into_owned(),
                exported_rel_path: dst_rel,
                exported_uri,
                size_bytes: copied_bytes,
            });
        }

        // 5. Write `dados.xml` (UTF-8 without BOM, CRLF) in staging and re-verify on disk.
        let stage_xml_path = stage_path.join("dados.xml");
        {
            let mut f = BufWriter::new(
                File::create(&stage_xml_path)
                    .map_err(|e| AppError::io("criar dados.xml temporário", e))?,
            );
            f.write_all(plan.xml_content.as_bytes())
                .map_err(|e| AppError::io("gravar dados.xml temporário", e))?;
            f.flush()
                .map_err(|e| AppError::io("finalizar escrita de dados.xml", e))?;
        }
        let staged_xml_check = std::fs::read_to_string(&stage_xml_path)
            .map_err(|e| AppError::io("reler dados.xml temporário", e))?;
        xml_engine.validate_xml_structure(&staged_xml_check, plan.expected_datasets)?;

        // 6. Write `manifest.json` in staging.
        let now = utc_now_rfc3339();
        let manifest = ExportManifest {
            project_id: plan.project_id.clone(),
            project_name: plan.project_name.clone(),
            path_strategy: plan.path_strategy,
            dataset_count: plan.expected_datasets,
            export_dir: crate::infrastructure::xml::path_to_native_absolute(&plan.target_dir),
            exported_at: now.clone(),
            updated_at: now,
            asset_mappings: manifest_assets,
        };
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| AppError::serialization(format!("Falha ao gerar manifest.json: {e}")))?;
        std::fs::write(stage_path.join("manifest.json"), manifest_json.as_bytes())
            .map_err(|e| AppError::io("gravar manifest.json temporário", e))?;

        // 7. Write `relatorio.csv` in staging.
        let stage_csv_path = stage_path.join("relatorio.csv");
        {
            let mut csv_writer = csv::WriterBuilder::new()
                .terminator(csv::Terminator::CRLF)
                .from_path(&stage_csv_path)
                .map_err(|e| AppError::export(format!("Falha ao criar relatorio.csv: {e}")))?;
            csv_writer
                .write_record([
                    "record_id",
                    "dataset_name",
                    "status",
                    "imagens_exportadas",
                    "avisos",
                ])
                .map_err(|e| AppError::export(format!("Falha ao gravar cabeçalho CSV: {e}")))?;

            for row in &plan.report_rows {
                csv_writer
                    .write_record([
                        row.record_id.as_str(),
                        row.dataset_name.as_str(),
                        row.status.as_str(),
                        &row.mapped_assets.join("; "),
                        &row.warnings.join(" | "),
                    ])
                    .map_err(|e| AppError::export(format!("Falha ao gravar linha CSV: {e}")))?;
            }
            csv_writer
                .flush()
                .map_err(|e| AppError::io("finalizar escrita de relatorio.csv", e))?;
        }

        // 8. Commit atomically into target_dir (move staged entries into target_dir).
        let final_xml = plan.target_dir.join("dados.xml");
        let final_images = plan.target_dir.join("imagens");
        let final_manifest = plan.target_dir.join("manifest.json");
        let final_csv = plan.target_dir.join("relatorio.csv");

        if overwrite_confirmed {
            if final_images.exists() {
                std::fs::remove_dir_all(&final_images)
                    .map_err(|e| AppError::io("limpar pasta imagens/ anterior", e))?;
            }
            for f in [&final_xml, &final_manifest, &final_csv] {
                if f.exists() {
                    std::fs::remove_file(f)
                        .map_err(|e| AppError::io("substituir arquivo de exportação anterior", e))?;
                }
            }
        }

        move_entry(&stage_images, &final_images)?;
        move_entry(&stage_xml_path, &final_xml)?;
        move_entry(&stage_path.join("manifest.json"), &final_manifest)?;
        move_entry(&stage_csv_path, &final_csv)?;

        // Explicitly drop and delete empty TempDir so no `.aivm_stage_*` survives.
        stage_dir
            .close()
            .map_err(|e| AppError::io("remover diretório temporário de exportação", e))?;

        Ok(ExportSummary {
            target_dir: plan.target_dir.clone(),
            dados_xml_path: final_xml,
            images_dir: final_images,
            manifest_path: final_manifest,
            report_csv_path: final_csv,
            exported_datasets: plan.expected_datasets,
            exported_assets: plan.asset_copies.len(),
            total_asset_bytes,
        })
    }
}

fn move_entry(src: &Path, dst: &Path) -> AppResult<()> {
    std::fs::rename(src, dst)
        .map_err(|e| AppError::io(format!("publicar '{}'", dst.display()), e))
}
