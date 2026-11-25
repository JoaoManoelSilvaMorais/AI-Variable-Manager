use std::fs;
use std::path::{Path, PathBuf};

use ai_variable_manager::domain::project::ProjectMode;
use ai_variable_manager::domain::variable::VariableKind;
use ai_variable_manager::domain::xml_model::TemplateVariableTrait;
use ai_variable_manager::AppServices;
use tempfile::tempdir;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

fn read_fixture(name: &str) -> String {
    let path = fixtures_dir().join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Falha ao ler fixture '{}': {e}", path.display()))
}

fn minimal_1x1_png_bytes() -> &'static [u8] {
    &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
        0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
        0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
        0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0, 0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF, 0x89, 0x99,
        0x3D, 0x1D, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

fn assert_crlf_utf8_no_bom(xml: &str) {
    assert!(!xml.starts_with('\u{FEFF}'), "XML não deve conter BOM UTF-8");
    assert!(!xml.as_bytes().starts_with(&[0xEF, 0xBB, 0xBF]));
    let bytes = xml.as_bytes();
    for (idx, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            assert!(
                idx > 0 && bytes[idx - 1] == b'\r',
                "Quebra LF isolada encontrada no offset {idx}"
            );
        }
        if b == b'\r' {
            assert!(
                idx + 1 < bytes.len() && bytes[idx + 1] == b'\n',
                "Quebra CR isolada encontrada no offset {idx}"
            );
        }
    }
}

#[test]
fn inspect_all_reference_fixtures_offline() {
    let dir = tempdir().unwrap();
    let services = AppServices::new_with_db_path(&dir.path().join("compat.db")).unwrap();

    let secom = services
        .export_service
        .inspect_xml_template(&read_fixture("secom_reference.xml"))
        .expect("secom_reference.xml deve ser inspecionado");
    assert_eq!(secom.variables.len(), 3);
    assert!(secom.entity_map.contains_key("ns_vars"));
    assert!(secom.entity_map.contains_key("ns_flows"));

    let segunda = services
        .export_service
        .inspect_xml_template(&read_fixture("segunda_leva_variant.xml"))
        .expect("segunda_leva_variant.xml deve ser inspecionado");
    assert!(
        segunda.has_c2pa_metadata,
        "segunda_leva_variant.xml deve preservar/sinalizar bloco c2pa:manifest"
    );
    assert!(segunda.variables.len() >= 2);
    assert!(segunda.sample_datasets_count >= 1);

    let bombeiro = services
        .export_service
        .inspect_xml_template(&read_fixture("bombeiro_reference.xml"))
        .expect("bombeiro_reference.xml deve ser inspecionado");
    let foto_var = bombeiro
        .variables
        .iter()
        .find(|v| v.var_name == "foto")
        .expect("bombeiro deve conter variável foto");
    assert_eq!(foto_var.trait_kind, TemplateVariableTrait::TextContent);
    assert_eq!(foto_var.sample_uses_p_wrapper, Some(false));

    for name in ["template_text.xml", "template_image.xml", "template_mixed.xml"] {
        let parsed = services
            .export_service
            .inspect_xml_template(&read_fixture(name))
            .unwrap_or_else(|e| panic!("Falha em {name}: {e}"));
        assert!(
            !parsed.variables.is_empty(),
            "{name} deve declarar pelo menos uma variável"
        );
    }
}

#[test]
fn with_template_secom_and_c2pa_preserve_dtd_and_replace_datasets_atomically() {
    let dir = tempdir().unwrap();
    let services = AppServices::new_with_db_path(&dir.path().join("secom.db")).unwrap();
    let project = services
        .project_service
        .create_project("Compat SECOM", ProjectMode::WithTemplate)
        .unwrap();

    let tpl_raw = read_fixture("segunda_leva_variant.xml");
    let parsed = services
        .export_service
        .bind_xml_template(&project.id, Some(Path::new("segunda_leva_variant.xml")), &tpl_raw)
        .unwrap();

    let mut foto_var_id = None;
    let mut text_var_ids = Vec::new();
    for tv in &parsed.variables {
        let is_photo = tv.trait_kind == TemplateVariableTrait::Fileref
            || (tv.var_name.eq_ignore_ascii_case("foto") && tv.sample_uses_p_wrapper == Some(false));
        let kind = if is_photo {
            VariableKind::LinkedFile
        } else {
            VariableKind::Text
        };
        let v = services
            .project_service
            .create_variable(&project.id, &tv.var_name, Some(&tv.var_name), kind, false)
            .unwrap();
        services
            .project_service
            .update_variable_options(&project.id, &v.id, kind, false, Some(tv.var_name.clone()))
            .unwrap();
        if is_photo {
            foto_var_id = Some(v.id);
        } else {
            text_var_ids.push(v.id);
        }
    }
    let foto_var_id = foto_var_id.expect("Modelo deve possuir variável de foto");

    let photos_dir = dir.path().join("origem_fotos");
    fs::create_dir_all(&photos_dir).unwrap();
    let photo_file = photos_dir.join("José & Maria 'SECOM'.png");
    fs::write(&photo_file, minimal_1x1_png_bytes()).unwrap();

    let rec = services
        .data_service
        .add_record(&project.id, Some("Registro 1 & \"Especial\" <OK>"))
        .unwrap();
    services
        .data_service
        .update_cell(&project.id, &rec.id, &text_var_ids[0], "João <SECOM> & Cia 'Oficial'\nLinha 2")
        .unwrap();
    if let Some(second) = text_var_ids.get(1) {
        services
            .data_service
            .update_cell(&project.id, &rec.id, second, "Design \"Sênior\"")
            .unwrap();
    }
    services
        .photo_service
        .link_manual(&project.id, &rec.id, &foto_var_id, &photo_file)
        .unwrap();

    let export_dir = dir.path().join("crachás teste");
    let summary = services
        .export_service
        .export_package(&project.id, &export_dir, false)
        .unwrap();
    assert_eq!(summary.exported_datasets, 1);
    assert_eq!(summary.exported_assets, 1);

    let out_xml = fs::read_to_string(&summary.dados_xml_path).unwrap();
    assert_crlf_utf8_no_bom(&out_xml);
    let expected_img = format!("{}\\imagens\\asset_001.png", export_dir.display());
    for req in [
        "<!ENTITY ns_graphs",
        "<!ENTITY ns_vars",
        "<!ENTITY ns_imrep",
        "<!ENTITY ns_custom",
        "<!ENTITY ns_flows",
        "<!ENTITY ns_extend",
        "c2pa:manifest",
        "Registro 1 &amp; &quot;Especial&quot; &lt;OK&gt;",
        "João &lt;SECOM&gt; &amp; Cia &apos;Oficial&apos;",
        expected_img.as_str(),
    ] {
        assert!(out_xml.contains(req), "Saída XML deveria conter '{req}'");
    }
    assert!(!out_xml.contains("file://"), "XML não deve conter URI file://");
    assert!(!out_xml.contains("%20"), "XML não deve conter percent-encoding");

    // Revalidate prefix update after moving the exported folder to a new path with accents & spaces
    let moved_dir = dir.path().join("Pacote Movido (Crachás)");
    fs::rename(&export_dir, &moved_dir).unwrap();
    let report = services
        .export_service
        .update_exported_prefix(&moved_dir)
        .unwrap();
    assert_eq!(report.updated_references, 1);
    let updated_xml = fs::read_to_string(moved_dir.join("dados.xml")).unwrap();
    assert_crlf_utf8_no_bom(&updated_xml);
    let expected_moved = format!("{}\\imagens\\asset_001.png", moved_dir.display());
    assert!(
        updated_xml.contains(&expected_moved) && !updated_xml.contains("file://"),
        "Caminho atualizado deve usar formato nativo Windows: esperado '{expected_moved}'"
    );
}

#[test]
fn bombeiro_legacy_trait_textcontent_photo_emits_without_p_wrapper() {
    let dir = tempdir().unwrap();
    let services = AppServices::new_with_db_path(&dir.path().join("bombeiro.db")).unwrap();
    let project = services
        .project_service
        .create_project("Compat Bombeiro", ProjectMode::WithTemplate)
        .unwrap();

    let tpl_raw = read_fixture("bombeiro_reference.xml");
    let parsed = services
        .export_service
        .bind_xml_template(&project.id, None, &tpl_raw)
        .unwrap();

    let mut mapped_text_ids = Vec::new();
    let mut foto_var_id = None;
    for tv in &parsed.variables {
        let is_photo = tv.var_name.eq_ignore_ascii_case("foto");
        let kind = if is_photo {
            VariableKind::LinkedFile
        } else {
            VariableKind::Text
        };
        let var = services
            .project_service
            .create_variable(&project.id, &tv.var_name, Some(&tv.var_name), kind, !is_photo)
            .unwrap();
        services
            .project_service
            .update_variable_options(&project.id, &var.id, kind, !is_photo, Some(tv.var_name.clone()))
            .unwrap();
        if is_photo {
            foto_var_id = Some(var.id);
        } else {
            mapped_text_ids.push((tv.var_name.clone(), var.id));
        }
    }
    let foto_var_id = foto_var_id.expect("bombeiro_reference deve conter 'foto'");

    let photo_file = dir.path().join("bombeiro_01.png");
    fs::write(&photo_file, minimal_1x1_png_bytes()).unwrap();

    let rec = services.data_service.add_record(&project.id, None).unwrap();
    for (tag, var_id) in &mapped_text_ids {
        services
            .data_service
            .update_cell(&project.id, &rec.id, var_id, &format!("Valor {tag}"))
            .unwrap();
    }
    services
        .photo_service
        .link_manual(&project.id, &rec.id, &foto_var_id, &photo_file)
        .unwrap();

    let out_dir = dir.path().join("saida_bombeiro");
    let summary = services
        .export_service
        .export_package(&project.id, &out_dir, false)
        .unwrap();
    let xml = fs::read_to_string(&summary.dados_xml_path).unwrap();
    assert_crlf_utf8_no_bom(&xml);
    for (tag, _) in &mapped_text_ids {
        assert!(xml.contains(&format!("<{tag}><p>Valor {tag}</p></{tag}>")));
    }
    let expected_photo = format!("<foto>{}\\imagens\\asset_001.png</foto>", out_dir.display());
    assert!(xml.contains(&expected_photo));
    assert!(!xml.contains("<foto><p>"));
    assert!(!xml.contains("file://"));
}

#[test]
#[ignore = "Requer acesso somente-leitura ao compartilhamento LAN real de referência"]
fn real_share_xmls_read_only_compatibility() {
    let share_dir = Path::new(r"\\192.168.0.12\semuce\06.0 - DESIGNER\2026\João Manoel\Outubro\20\crachás backup\Crachas");
    assert!(share_dir.is_dir(), "Compartilhamento não acessível: {}", share_dir.display());

    let db_dir = tempdir().unwrap();
    let services = AppServices::new_with_db_path(&db_dir.path().join("share_readonly.db")).unwrap();

    let mut inspected_count = 0usize;
    for entry in fs::read_dir(share_dir).unwrap().flatten() {
        let path = entry.path();
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
        {
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("Falha somente-leitura em '{}': {e}", path.display()));
            let parsed = services
                .export_service
                .inspect_xml_template(&content)
                .unwrap_or_else(|e| panic!("Falha ao analisar '{}': {e}", path.display()));
            println!(
                "OK XML real '{}': varSet={:?}, vars={}, datasets={}, c2pa={}",
                path.file_name().unwrap().to_string_lossy(),
                parsed.var_set_name,
                parsed.variables.len(),
                parsed.sample_datasets_count,
                parsed.has_c2pa_metadata
            );
            assert!(
                !parsed.variables.is_empty(),
                "XML real '{}' não apresentou variáveis",
                path.display()
            );

            // Round-trip offline bind + preview + temp-dir export (zero mutation of share)
            let proj = services
                .project_service
                .create_project(
                    &format!("Share Check {}", inspected_count + 1),
                    ProjectMode::WithTemplate,
                )
                .unwrap();
            services
                .export_service
                .bind_xml_template(&proj.id, None, &content)
                .unwrap();
            let photo_path = db_dir.path().join("sample_photo.png");
            if !photo_path.exists() {
                fs::write(&photo_path, minimal_1x1_png_bytes()).unwrap();
            }
            let rec = services
                .data_service
                .add_record(&proj.id, Some("Teste Share 1"))
                .unwrap();
            for tv in &parsed.variables {
                let is_photo = tv.trait_kind == TemplateVariableTrait::Fileref
                    || tv.sample_uses_p_wrapper == Some(false)
                    || tv.var_name.to_ascii_lowercase().contains("foto");
                let kind = if is_photo {
                    VariableKind::LinkedFile
                } else {
                    VariableKind::Text
                };
                let var = services
                    .project_service
                    .create_variable(&proj.id, &tv.var_name, Some(&tv.var_name), kind, !is_photo)
                    .unwrap();
                services
                    .project_service
                    .update_variable_options(
                        &proj.id,
                        &var.id,
                        kind,
                        !is_photo,
                        Some(tv.var_name.clone()),
                    )
                    .unwrap();
                if is_photo {
                    services
                        .photo_service
                        .link_manual(&proj.id, &rec.id, &var.id, &photo_path)
                        .unwrap();
                } else {
                    services
                        .data_service
                        .update_cell(&proj.id, &rec.id, &var.id, "Teste <Illustrator> & Cia")
                        .unwrap();
                }
            }
            let out_dir = db_dir.path().join(format!("out_{inspected_count}"));
            let summary = services
                .export_service
                .export_package(&proj.id, &out_dir, false)
                .unwrap();
            let generated = fs::read_to_string(&summary.dados_xml_path).unwrap();
            assert_crlf_utf8_no_bom(&generated);
            services
                .export_service
                .validate_xml_structure(&generated, 1)
                .unwrap();
            inspected_count += 1;
        }
    }
    assert!(inspected_count >= 5, "Esperados pelo menos 5 XMLs de referência, encontrados {inspected_count}");
}
