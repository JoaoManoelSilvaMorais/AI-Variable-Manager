use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use quick_xml::XmlVersion;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::domain::ports::XmlEngine;
use crate::domain::project::ProjectMode;
use crate::domain::variable::VariableKind;
use crate::domain::xml_model::{
    ParsedXmlTemplate, PrefixUpdateReport, TemplateVariable, TemplateVariableTrait, XmlCellValue,
    XmlColumnBinding, XmlDatasetEntry, XmlExportSpec,
};
use crate::infrastructure::xml::path_resolver::{extract_asset_rel_path, format_asset_target};
use crate::shared::error::{AppError, AppResult};
use crate::shared::time::utc_now_rfc3339;

const REQUIRED_ADOBE_ENTITIES: &[&str] = &[
    "ns_graphs",
    "ns_vars",
    "ns_imrep",
    "ns_custom",
    "ns_flows",
    "ns_extend",
];

#[derive(Debug, Default, Clone, Copy)]
pub struct IllustratorXmlEngine;

impl IllustratorXmlEngine {
    pub fn new() -> Self {
        Self
    }
}

/// Escapes reserved XML characters (`&`, `<`, `>`, `"`, `'`) in text and attribute values.
pub fn escape_xml_reserved(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Normalizes line endings within text payloads so every newline in output is `\r\n` (CRLF).
fn normalize_newlines_to_crlf(input: &str) -> String {
    let lf = input.replace("\r\n", "\n").replace('\r', "\n");
    lf.replace('\n', "\r\n")
}

fn strip_utf8_bom(raw: &str) -> &str {
    raw.strip_prefix('\u{FEFF}').unwrap_or(raw)
}

fn extract_dtd_entities(dt_text: &str) -> HashMap<String, String> {
    let mut entities = HashMap::new();
    let mut rest = dt_text;
    while let Some(idx) = rest.find("<!ENTITY") {
        rest = &rest[idx + "<!ENTITY".len()..];
        let trimmed = rest.trim_start();
        let name_end = trimmed
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(trimmed.len());
        let name = &trimmed[..name_end];
        let after_name = trimmed[name_end..].trim_start();
        if let Some(quote) = after_name.chars().next().filter(|c| *c == '"' || *c == '\'') {
            let inner = &after_name[quote.len_utf8()..];
            if let Some(end_q) = inner.find(quote) {
                let val = &inner[..end_q];
                if !name.is_empty() {
                    entities.insert(name.to_string(), val.to_string());
                }
                rest = &inner[end_q + quote.len_utf8()..];
                continue;
            }
        }
        rest = after_name;
    }
    entities
}

fn parse_trait(raw: &str) -> TemplateVariableTrait {
    match raw.trim().to_ascii_lowercase().as_str() {
        "textcontent" => TemplateVariableTrait::TextContent,
        "fileref" => TemplateVariableTrait::Fileref,
        "visibility" => TemplateVariableTrait::Visibility,
        "graphdata" => TemplateVariableTrait::GraphData,
        other => TemplateVariableTrait::Unknown(other.to_string()),
    }
}

fn is_sample_datasets_tag(tag: &str) -> bool {
    tag == "v:sampleDataSets" || tag == "sampleDataSets"
}

fn is_sample_dataset_tag(tag: &str) -> bool {
    tag == "v:sampleDataSet" || tag == "sampleDataSet"
}

fn decode_attrs<'a>(
    e: &'a quick_xml::events::BytesStart<'a>,
    entities: &HashMap<String, String>,
) -> AppResult<HashMap<String, String>> {
    let mut map = HashMap::new();
    for attr_res in e.attributes() {
        let attr = attr_res
            .map_err(|err| AppError::xml(format!("Atributo XML malformado: {err}")))?;
        let key = String::from_utf8(attr.key.as_ref().to_vec())
            .map_err(|err| AppError::xml(format!("Chave de atributo não UTF-8: {err}")))?;
        if attr.value.contains(&b'<') {
            return Err(AppError::xml(format!(
                "Caractere '<' não escapado no atributo '{key}'."
            )));
        }
        let val = attr
            .decoded_and_normalized_value_with(
                XmlVersion::Implicit1_0,
                e.decoder(),
                128,
                |ent| {
                    resolve_predefined_entity(ent)
                        .or_else(|| entities.get(ent).map(String::as_str))
                },
            )
            .map_err(|err| AppError::xml(format!("Entidade ou escape inválido em '{key}': {err}")))?;
        map.insert(key, val.into_owned());
    }
    Ok(map)
}

fn parse_template_internal(xml_raw: &str) -> AppResult<ParsedXmlTemplate> {
    let clean = strip_utf8_bom(xml_raw);
    let mut reader = Reader::from_str(clean);
    reader.config_mut().check_end_names = true;
    reader.config_mut().check_comments = true;
    reader.config_mut().allow_dangling_amp = false;
    reader.config_mut().allow_unmatched_ends = false;

    let mut entity_map = HashMap::new();
    let mut tag_stack: Vec<String> = Vec::new();
    let mut var_set_name: Option<String> = None;
    let mut variables: Vec<TemplateVariable> = Vec::new();
    let mut var_p_usage: HashMap<String, bool> = HashMap::new();
    let mut saw_variables = false;
    let mut saw_sample_datasets = false;
    let mut sample_datasets_count = 0usize;
    let mut sample_dataset_prefix = "v:sampleDataSet".to_string();
    let mut span_start: Option<usize> = None;
    let mut span_end: Option<usize> = None;
    let mut has_c2pa_metadata = false;

    let mut active_sample_field: Option<String> = None;
    let mut active_field_saw_p = false;

    loop {
        let pos_before = reader.buffer_position() as usize;
        let event = reader
            .read_event()
            .map_err(|err| AppError::xml(format!("XML inválido: {err}")))?;

        match event {
            Event::DocType(dt) => {
                let dt_text = dt
                    .decode()
                    .map_err(|err| AppError::xml(format!("Falha ao decodificar DOCTYPE: {err}")))?;
                entity_map = extract_dtd_entities(&dt_text);
            }
            Event::Start(e) => {
                let tag = String::from_utf8(e.name().as_ref().to_vec())
                    .map_err(|err| AppError::xml(format!("Tag XML não UTF-8: {err}")))?;
                let attrs = decode_attrs(&e, &entity_map)?;

                if tag.starts_with("c2pa:") {
                    has_c2pa_metadata = true;
                } else if tag == "svg" && attrs.keys().any(|k| k.contains("c2pa")) {
                    has_c2pa_metadata = true;
                } else if tag == "variableSet" {
                    if let Some(name) = attrs.get("varSetName") {
                        var_set_name = Some(name.clone());
                    }
                } else if tag == "variables" {
                    saw_variables = true;
                } else if tag == "variable" {
                    if let Some(var_name) = attrs.get("varName") {
                        let trait_kind = attrs
                            .get("trait")
                            .map(|t| parse_trait(t))
                            .unwrap_or(TemplateVariableTrait::TextContent);
                        variables.push(TemplateVariable {
                            var_name: var_name.clone(),
                            trait_kind,
                            category: attrs.get("category").cloned(),
                            sample_uses_p_wrapper: None,
                        });
                    }
                } else if is_sample_datasets_tag(&tag) {
                    saw_sample_datasets = true;
                    span_start = Some(reader.buffer_position() as usize);
                } else if is_sample_dataset_tag(&tag) {
                    sample_datasets_count += 1;
                    sample_dataset_prefix = tag.clone();
                } else if tag_stack.last().is_some_and(|p| is_sample_dataset_tag(p)) {
                    active_sample_field = Some(tag.clone());
                    active_field_saw_p = false;
                } else if active_sample_field.is_some() && tag == "p" {
                    active_field_saw_p = true;
                }

                tag_stack.push(tag);
            }
            Event::Empty(e) => {
                let tag = String::from_utf8(e.name().as_ref().to_vec())
                    .map_err(|err| AppError::xml(format!("Tag XML não UTF-8: {err}")))?;
                let attrs = decode_attrs(&e, &entity_map)?;

                if tag.starts_with("c2pa:") {
                    has_c2pa_metadata = true;
                } else if tag == "variables" {
                    saw_variables = true;
                } else if tag == "variable" {
                    if let Some(var_name) = attrs.get("varName") {
                        let trait_kind = attrs
                            .get("trait")
                            .map(|t| parse_trait(t))
                            .unwrap_or(TemplateVariableTrait::TextContent);
                        variables.push(TemplateVariable {
                            var_name: var_name.clone(),
                            trait_kind,
                            category: attrs.get("category").cloned(),
                            sample_uses_p_wrapper: None,
                        });
                    }
                } else if is_sample_datasets_tag(&tag) {
                    return Err(AppError::xml(
                        "O elemento <v:sampleDataSets/> autocontido não possui corpo editável; salve o XML-base pelo painel Variables do Illustrator.",
                    ));
                } else if is_sample_dataset_tag(&tag) {
                    sample_datasets_count += 1;
                    sample_dataset_prefix = tag;
                } else if active_sample_field.is_some() && tag == "p" {
                    active_field_saw_p = true;
                } else if tag_stack.last().is_some_and(|p| is_sample_dataset_tag(p)) {
                    var_p_usage.entry(tag).or_insert(false);
                }
            }
            Event::GeneralRef(r) => {
                if r.is_char_ref() {
                    r.resolve_char_ref().map_err(|err| {
                        AppError::xml(format!("Referência de caractere XML inválida: {err}"))
                    })?;
                } else {
                    let name = r.decode().map_err(|err| {
                        AppError::xml(format!("Entidade XML malformada: {err}"))
                    })?;
                    if resolve_predefined_entity(&name).is_none()
                        && !entity_map.contains_key(name.as_ref())
                    {
                        return Err(AppError::xml(format!(
                            "Entidade XML desconhecida '&{name};'."
                        )));
                    }
                }
            }
            Event::End(e) => {
                let tag = String::from_utf8(e.name().as_ref().to_vec())
                    .map_err(|err| AppError::xml(format!("Tag de fechamento não UTF-8: {err}")))?;
                let popped = tag_stack.pop().ok_or_else(|| {
                    AppError::xml(format!("Tag de fechamento inesperada '</{tag}>'."))
                })?;
                if popped != tag {
                    return Err(AppError::xml(format!(
                        "Tag de fechamento incompatível: esperado '</{popped}>', encontrado '</{tag}>'."
                    )));
                }

                if let Some(ref field_name) = active_sample_field
                    && *field_name == tag
                    && tag_stack.last().is_some_and(|p| is_sample_dataset_tag(p))
                {
                    var_p_usage
                        .entry(field_name.clone())
                        .and_modify(|seen| *seen |= active_field_saw_p)
                        .or_insert(active_field_saw_p);
                    active_sample_field = None;
                    active_field_saw_p = false;
                } else if is_sample_datasets_tag(&tag) {
                    span_end = Some(pos_before);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if !tag_stack.is_empty() {
        return Err(AppError::xml(format!(
            "Tags XML não fechadas no modelo: {tag_stack:?}"
        )));
    }
    if !saw_variables {
        return Err(AppError::xml(
            "O XML-base não contém o bloco obrigatório <variables>.",
        ));
    }
    if !saw_sample_datasets {
        return Err(AppError::xml(
            "O XML-base não contém o bloco obrigatório <v:sampleDataSets>.",
        ));
    }

    for v in &mut variables {
        v.sample_uses_p_wrapper = var_p_usage.get(&v.var_name).copied();
    }

    let (start, end) = match (span_start, span_end) {
        (Some(s), Some(e)) if s <= e => (s, e),
        _ => {
            return Err(AppError::xml(
                "Não foi possível localizar o intervalo interno de <v:sampleDataSets>.",
            ));
        }
    };

    Ok(ParsedXmlTemplate {
        var_set_name,
        variables,
        entity_map,
        sample_datasets_count,
        sample_datasets_inner_span: (start, end),
        sample_dataset_prefix,
        has_c2pa_metadata,
    })
}

fn detect_template_indent(prefix: &str, closing_line_before_end: &str) -> (String, String) {
    // Closing line indent is trailing spaces after the last '\n' before </v:sampleDataSets>
    let parent_indent = closing_line_before_end
        .rsplit('\n')
        .next()
        .unwrap_or("    ")
        .trim_end_matches('\r')
        .to_string();

    // Detect indent step from prefix (2 spaces vs 4 spaces)
    let step = if prefix.contains("\n  <variableSet") || parent_indent.len() == 2 {
        "  "
    } else {
        "    "
    };

    let ds_indent = format!("{parent_indent}{step}");
    (parent_indent, ds_indent)
}

fn render_datasets_body(
    dataset_tag: &str,
    ds_indent: &str,
    field_indent: &str,
    columns: &[XmlColumnBinding],
    datasets: &[XmlDatasetEntry],
) -> AppResult<String> {
    let mut out = String::new();
    for entry in datasets {
        if entry.values.len() != columns.len() {
            return Err(AppError::xml(format!(
                "Quantidade de valores ({}) diverge do número de colunas ({}) no registro '{}'.",
                entry.values.len(),
                columns.len(),
                entry.dataset_name
            )));
        }
        let escaped_ds = escape_xml_reserved(&entry.dataset_name);
        out.push_str(&format!(
            "{ds_indent}<{dataset_tag} dataSetName=\"{escaped_ds}\">\r\n"
        ));

        for (col, cell) in columns.iter().zip(entry.values.iter()) {
            let tag = &col.xml_tag_name;
            match (col.kind, cell) {
                (VariableKind::Text, XmlCellValue::Text(txt)) => {
                    let escaped = escape_xml_reserved(&normalize_newlines_to_crlf(txt));
                    out.push_str(&format!("{field_indent}<{tag}><p>{escaped}</p></{tag}>\r\n"));
                }
                (VariableKind::LinkedFile, XmlCellValue::LinkedFile(path_str)) => {
                    let escaped = escape_xml_reserved(path_str.trim());
                    out.push_str(&format!("{field_indent}<{tag}>{escaped}</{tag}>\r\n"));
                }
                (VariableKind::Text, XmlCellValue::LinkedFile(txt)) => {
                    let escaped = escape_xml_reserved(&normalize_newlines_to_crlf(txt));
                    out.push_str(&format!("{field_indent}<{tag}><p>{escaped}</p></{tag}>\r\n"));
                }
                (VariableKind::LinkedFile, XmlCellValue::Text(path_str)) => {
                    let escaped = escape_xml_reserved(path_str.trim());
                    out.push_str(&format!("{field_indent}<{tag}>{escaped}</{tag}>\r\n"));
                }
            }
        }

        out.push_str(&format!("{ds_indent}</{dataset_tag}>\r\n"));
    }
    Ok(out)
}

fn render_standard_illustrator_envelope(spec: &XmlExportSpec) -> AppResult<String> {
    let var_set_name = escape_xml_reserved(if spec.var_set_name.trim().is_empty() {
        "ConjuntoVariaveis"
    } else {
        spec.var_set_name.trim()
    });

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n");
    out.push_str("<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 20001102//EN\"    \"http://www.w3.org/TR/2000/CR-SVG-20001102/DTD/svg-20001102.dtd\" [\r\n");
    out.push_str("    <!ENTITY ns_graphs \"http://ns.adobe.com/Graphs/1.0/\">\r\n");
    out.push_str("    <!ENTITY ns_vars \"http://ns.adobe.com/Variables/1.0/\">\r\n");
    out.push_str("    <!ENTITY ns_imrep \"http://ns.adobe.com/ImageReplacement/1.0/\">\r\n");
    out.push_str("    <!ENTITY ns_custom \"http://ns.adobe.com/GenericCustomNamespace/1.0/\">\r\n");
    out.push_str("    <!ENTITY ns_flows \"http://ns.adobe.com/Flows/1.0/\">\r\n");
    out.push_str("    <!ENTITY ns_extend \"http://ns.adobe.com/Extensibility/1.0/\">\r\n");
    out.push_str("]>\r\n");
    out.push_str("<svg>\r\n");
    out.push_str("<variableSets xmlns=\"&ns_vars;\">\r\n");
    out.push_str(&format!(
        "    <variableSet locked=\"none\" varSetName=\"{var_set_name}\">\r\n"
    ));
    out.push_str("        <variables>\r\n");

    for col in &spec.columns {
        let var_name = escape_xml_reserved(&col.xml_tag_name);
        let (category, trait_str) = match col.kind {
            VariableKind::Text => ("&ns_flows;", "textcontent"),
            VariableKind::LinkedFile => ("&ns_imrep;", "fileref"),
        };
        out.push_str(&format!(
            "            <variable category=\"{category}\" trait=\"{trait_str}\" varName=\"{var_name}\"></variable>\r\n"
        ));
    }

    out.push_str("        </variables>\r\n");
    out.push_str("        <v:sampleDataSets xmlns:v=\"&ns_vars;\" xmlns=\"&ns_custom;\">\r\n");

    let body = render_datasets_body(
        "v:sampleDataSet",
        "            ",
        "                ",
        &spec.columns,
        &spec.datasets,
    )?;
    out.push_str(&body);
    out.push_str("        </v:sampleDataSets>\r\n");
    out.push_str("    </variableSet>\r\n");
    out.push_str("</variableSets>\r\n");
    out.push_str("</svg>\r\n");

    Ok(out)
}

fn splice_template_xml(tpl_raw: &str, spec: &XmlExportSpec) -> AppResult<String> {
    let clean = strip_utf8_bom(tpl_raw);
    let parsed = parse_template_internal(clean)?;
    let (inner_start, inner_end) = parsed.sample_datasets_inner_span;

    let prefix = &clean[..inner_start];
    let suffix = &clean[inner_end..];
    let inner_original = &clean[inner_start..inner_end];

    let (parent_indent, ds_indent) = detect_template_indent(prefix, inner_original);
    let step = if ds_indent.len() > parent_indent.len() {
        &ds_indent[parent_indent.len()..]
    } else {
        "    "
    };
    let field_indent = format!("{ds_indent}{step}");

    let body = render_datasets_body(
        &parsed.sample_dataset_prefix,
        &ds_indent,
        &field_indent,
        &spec.columns,
        &spec.datasets,
    )?;

    let mut out = String::with_capacity(clean.len() + body.len());
    out.push_str(prefix);
    out.push_str("\r\n");
    out.push_str(&body);
    out.push_str(&parent_indent);
    out.push_str(suffix);

    // Ensure final trailing newline and CRLF consistency if template lacked trailing newline
    if !out.ends_with("\r\n") {
        if out.ends_with('\n') {
            out.pop();
            out.push_str("\r\n");
        } else {
            out.push_str("\r\n");
        }
    }

    Ok(out)
}

fn validate_generated_xml_structure(xml_raw: &str, expected_datasets: usize) -> AppResult<()> {
    let clean = strip_utf8_bom(xml_raw);
    if !clean.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>") {
        return Err(AppError::xml(
            "O XML gerado não inicia com a declaração UTF-8 padrão.",
        ));
    }

    let mut reader = Reader::from_str(clean);
    reader.config_mut().check_end_names = true;
    reader.config_mut().check_comments = true;
    reader.config_mut().allow_dangling_amp = false;
    reader.config_mut().allow_unmatched_ends = false;

    let mut entity_map = HashMap::new();
    let mut saw_doctype = false;
    let mut tag_stack: Vec<String> = Vec::new();
    let mut dataset_count = 0usize;
    let mut dataset_names: HashSet<String> = HashSet::new();

    loop {
        match reader
            .read_event()
            .map_err(|err| AppError::xml(format!("Estrutura XML inválida: {err}")))?
        {
            Event::DocType(dt) => {
                saw_doctype = true;
                let dt_text = dt
                    .decode()
                    .map_err(|err| AppError::xml(format!("Falha ao ler DOCTYPE: {err}")))?;
                entity_map = extract_dtd_entities(&dt_text);
                for req in REQUIRED_ADOBE_ENTITIES {
                    if !entity_map.contains_key(*req) {
                        return Err(AppError::xml(format!(
                            "Entidade Adobe obrigatória '&{req};' ausente no DOCTYPE."
                        )));
                    }
                }
            }
            Event::Start(e) => {
                let tag = String::from_utf8(e.name().as_ref().to_vec())
                    .map_err(|err| AppError::xml(format!("Tag não UTF-8: {err}")))?;
                let attrs = decode_attrs(&e, &entity_map)?;
                if is_sample_dataset_tag(&tag) {
                    dataset_count += 1;
                    let ds_name = attrs.get("dataSetName").ok_or_else(|| {
                        AppError::xml("Elemento sampleDataSet sem atributo 'dataSetName'.")
                    })?;
                    if ds_name.is_empty() {
                        return Err(AppError::xml(
                            "Atributo 'dataSetName' não pode estar vazio.",
                        ));
                    }
                    if !dataset_names.insert(ds_name.clone()) {
                        return Err(AppError::xml(format!(
                            "Atributo 'dataSetName' duplicado no XML: '{ds_name}'."
                        )));
                    }
                }
                tag_stack.push(tag);
            }
            Event::Empty(e) => {
                let tag = String::from_utf8(e.name().as_ref().to_vec())
                    .map_err(|err| AppError::xml(format!("Tag não UTF-8: {err}")))?;
                let attrs = decode_attrs(&e, &entity_map)?;
                if is_sample_dataset_tag(&tag) {
                    dataset_count += 1;
                    let ds_name = attrs.get("dataSetName").ok_or_else(|| {
                        AppError::xml("Elemento sampleDataSet sem atributo 'dataSetName'.")
                    })?;
                    if ds_name.is_empty() || !dataset_names.insert(ds_name.clone()) {
                        return Err(AppError::xml(format!(
                            "Atributo 'dataSetName' inválido ou duplicado: '{ds_name}'."
                        )));
                    }
                }
            }
            Event::Text(t) => {
                t.xml10_content().map_err(|err| {
                    AppError::xml(format!("Conteúdo textual XML 1.0 inválido: {err}"))
                })?;
            }
            Event::GeneralRef(r) => {
                if r.is_char_ref() {
                    r.resolve_char_ref().map_err(|err| {
                        AppError::xml(format!("Referência numérica XML inválida: {err}"))
                    })?;
                } else {
                    let name = r.decode().map_err(|err| {
                        AppError::xml(format!("Entidade XML malformada: {err}"))
                    })?;
                    if resolve_predefined_entity(&name).is_none()
                        && !entity_map.contains_key(name.as_ref())
                    {
                        return Err(AppError::xml(format!(
                            "Entidade XML não declarada '&{name};'."
                        )));
                    }
                }
            }
            Event::End(e) => {
                let tag = String::from_utf8(e.name().as_ref().to_vec())
                    .map_err(|err| AppError::xml(format!("Tag não UTF-8: {err}")))?;
                let popped = tag_stack.pop().ok_or_else(|| {
                    AppError::xml(format!("Tag de fechamento inesperada '</{tag}>'."))
                })?;
                if popped != tag {
                    return Err(AppError::xml(format!(
                        "Tag de fechamento incompatível: '{popped}' vs '{tag}'."
                    )));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if !tag_stack.is_empty() {
        return Err(AppError::xml(format!(
            "Tags XML não fechadas: {tag_stack:?}"
        )));
    }
    if !saw_doctype {
        return Err(AppError::xml(
            "O XML gerado não contém o cabeçalho <!DOCTYPE svg ...> do Adobe Illustrator.",
        ));
    }
    if dataset_count != expected_datasets {
        return Err(AppError::xml(format!(
            "Contagem de conjuntos de dados diverge: esperado {expected_datasets}, encontrado {dataset_count}."
        )));
    }

    Ok(())
}

impl XmlEngine for IllustratorXmlEngine {
    fn inspect_template(&self, xml_raw: &str) -> AppResult<ParsedXmlTemplate> {
        parse_template_internal(xml_raw)
    }

    fn generate_xml(&self, spec: &XmlExportSpec) -> AppResult<String> {
        match (spec.mode, spec.template_raw.as_deref()) {
            (ProjectMode::WithTemplate, Some(tpl)) => splice_template_xml(tpl, spec),
            (ProjectMode::WithTemplate, None) => Err(AppError::xml(
                "Projeto em Modo XML com Modelo não possui XML-base associado.",
            )),
            (ProjectMode::WithoutTemplate, _) => render_standard_illustrator_envelope(spec),
        }
    }

    fn validate_xml_structure(&self, xml_raw: &str, expected_datasets: usize) -> AppResult<()> {
        validate_generated_xml_structure(xml_raw, expected_datasets)
    }

    fn update_export_prefix(&self, export_dir: &Path) -> AppResult<PrefixUpdateReport> {
        let dados_path = export_dir.join("dados.xml");
        let manifest_path = export_dir.join("manifest.json");

        if !dados_path.is_file() {
            return Err(AppError::not_found("dados.xml", dados_path.display().to_string()));
        }
        if !manifest_path.is_file() {
            return Err(AppError::not_found(
                "manifest.json",
                manifest_path.display().to_string(),
            ));
        }

        let manifest_raw = std::fs::read_to_string(&manifest_path)
            .map_err(|e| AppError::io("ler manifest.json", e))?;
        let mut manifest_json: serde_json::Value = serde_json::from_str(&manifest_raw)
            .map_err(|e| AppError::serialization(format!("manifest.json inválido: {e}")))?;

        let expected_datasets = manifest_json
            .get("dataset_count")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        // Verify all mapped assets in manifest.json exist physically in export_dir/imagens/.
        let mut verified_assets = 0usize;
        let mut missing_assets: Vec<PathBuf> = Vec::new();
        if let Some(mappings) = manifest_json
            .get("asset_mappings")
            .and_then(|v| v.as_array())
        {
            for item in mappings {
                if let Some(rel) = item.get("exported_rel_path").and_then(|v| v.as_str()) {
                    let asset_file = export_dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
                    if asset_file.is_file() {
                        verified_assets += 1;
                    } else {
                        missing_assets.push(asset_file);
                    }
                }
            }
        }
        if !missing_assets.is_empty() {
            return Err(AppError::export(format!(
                "Não é possível atualizar os caminhos: {} arquivo(s) ausente(s) em imagens/.",
                missing_assets.len()
            )));
        }

        let xml_raw = std::fs::read_to_string(&dados_path)
            .map_err(|e| AppError::io("ler dados.xml", e))?;
        let parsed = parse_template_internal(&xml_raw)?;
        validate_generated_xml_structure(
            &xml_raw,
            expected_datasets.unwrap_or(parsed.sample_datasets_count),
        )?;

        // Surgically replace only the inner text of LinkedFile fields inside <v:sampleDataSet>.
        let clean_xml = strip_utf8_bom(&xml_raw);
        let (rewritten_xml, updated_refs, missing_xml_assets) =
            rewrite_xml_asset_paths(clean_xml, export_dir)?;
        if !missing_xml_assets.is_empty() {
            return Err(AppError::export(format!(
                "Ativo referenciado em dados.xml não encontrado em imagens/: {}",
                missing_xml_assets[0].display()
            )));
        }

        validate_generated_xml_structure(
            &rewritten_xml,
            expected_datasets.unwrap_or(parsed.sample_datasets_count),
        )?;

        // Update manifest.json fields
        manifest_json["path_strategy"] = serde_json::Value::String("absolute_path".into());
        manifest_json["export_dir"] =
            serde_json::Value::String(crate::infrastructure::xml::path_to_native_absolute(export_dir));
        manifest_json["updated_at"] = serde_json::Value::String(utc_now_rfc3339());
        if let Some(mappings) = manifest_json
            .get_mut("asset_mappings")
            .and_then(|v| v.as_array_mut())
        {
            for item in mappings {
                if let Some(rel) = item
                    .get("exported_rel_path")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                {
                    let new_target = format_asset_target(export_dir, &rel);
                    item["exported_uri"] = serde_json::Value::String(new_target);
                }
            }
        }
        let manifest_out = serde_json::to_string_pretty(&manifest_json)
            .map_err(|e| AppError::serialization(format!("Erro ao serializar manifest.json: {e}")))?;

        // Atomic write via temp files in export_dir
        atomic_replace_file(&dados_path, rewritten_xml.as_bytes())?;
        atomic_replace_file(&manifest_path, manifest_out.as_bytes())?;

        Ok(PrefixUpdateReport {
            export_dir: export_dir.to_path_buf(),
            updated_references: updated_refs,
            verified_assets,
            missing_assets: Vec::new(),
            updated_dados_xml: dados_path,
            updated_manifest_json: manifest_path,
        })
    }
}

fn rewrite_xml_asset_paths(
    xml_str: &str,
    export_dir: &Path,
) -> AppResult<(String, usize, Vec<PathBuf>)> {
    let mut reader = Reader::from_str(xml_str);
    reader.config_mut().check_end_names = true;

    let mut entity_map = HashMap::new();
    let mut tag_stack: Vec<String> = Vec::new();
    let mut var_traits: HashMap<String, TemplateVariableTrait> = HashMap::new();

    struct ActiveField {
        tag_name: String,
        inner_start: usize,
        saw_p: bool,
    }
    let mut active_field: Option<ActiveField> = None;
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    let mut missing_assets: Vec<PathBuf> = Vec::new();

    loop {
        let pos_before = reader.buffer_position() as usize;
        match reader
            .read_event()
            .map_err(|e| AppError::xml(format!("Falha ao reler dados.xml: {e}")))?
        {
            Event::DocType(dt) => {
                let dt_text = dt
                    .decode()
                    .map_err(|e| AppError::xml(format!("Falha DOCTYPE: {e}")))?;
                entity_map = extract_dtd_entities(&dt_text);
            }
            Event::Start(e) => {
                let tag = String::from_utf8(e.name().as_ref().to_vec())
                    .map_err(|err| AppError::xml(format!("Tag não UTF-8: {err}")))?;
                let attrs = decode_attrs(&e, &entity_map)?;

                if tag == "variable" {
                    if let (Some(vn), Some(tr)) = (attrs.get("varName"), attrs.get("trait")) {
                        var_traits.insert(vn.clone(), parse_trait(tr));
                    }
                } else if tag_stack.last().is_some_and(|p| is_sample_dataset_tag(p)) {
                    active_field = Some(ActiveField {
                        tag_name: tag.clone(),
                        inner_start: reader.buffer_position() as usize,
                        saw_p: false,
                    });
                } else if let Some(ref mut f) = active_field
                    && tag == "p"
                {
                    f.saw_p = true;
                }
                tag_stack.push(tag);
            }
            Event::Empty(e) => {
                let tag = String::from_utf8(e.name().as_ref().to_vec())
                    .map_err(|err| AppError::xml(format!("Tag não UTF-8: {err}")))?;
                let attrs = decode_attrs(&e, &entity_map)?;
                if tag == "variable" {
                    if let (Some(vn), Some(tr)) = (attrs.get("varName"), attrs.get("trait")) {
                        var_traits.insert(vn.clone(), parse_trait(tr));
                    }
                } else if let Some(ref mut f) = active_field
                    && tag == "p"
                {
                    f.saw_p = true;
                }
            }
            Event::End(e) => {
                let tag = String::from_utf8(e.name().as_ref().to_vec())
                    .map_err(|err| AppError::xml(format!("Tag não UTF-8: {err}")))?;
                tag_stack.pop();
                if let Some(f) = active_field.take() {
                    if f.tag_name == tag
                        && tag_stack.last().is_some_and(|p| is_sample_dataset_tag(p))
                    {
                        if !f.saw_p {
                            let inner_raw = &xml_str[f.inner_start..pos_before];
                            let trait_is_fileref = matches!(
                                var_traits.get(&f.tag_name),
                                Some(TemplateVariableTrait::Fileref)
                            );
                            if let Some(rel_path) = extract_asset_rel_path(inner_raw) {
                                let disk_path = export_dir
                                    .join(rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));
                                if !disk_path.is_file() {
                                    missing_assets.push(disk_path);
                                } else {
                                    let next_val = format_asset_target(export_dir, &rel_path);
                                    replacements.push((
                                        f.inner_start,
                                        pos_before,
                                        escape_xml_reserved(&next_val),
                                    ));
                                }
                            } else if trait_is_fileref && !inner_raw.trim().is_empty() {
                                missing_assets.push(PathBuf::from(inner_raw.trim()));
                            }
                        }
                    } else {
                        active_field = Some(f);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if !missing_assets.is_empty() {
        return Ok((String::new(), 0, missing_assets));
    }

    let mut out = String::with_capacity(xml_str.len() + 256);
    let mut cursor = 0usize;
    let count = replacements.len();
    for (start, end, rep) in replacements {
        out.push_str(&xml_str[cursor..start]);
        out.push_str(&rep);
        cursor = end;
    }
    out.push_str(&xml_str[cursor..]);
    Ok((out, count, Vec::new()))
}

fn atomic_replace_file(target_path: &Path, content: &[u8]) -> AppResult<()> {
    let parent = target_path
        .parent()
        .ok_or_else(|| AppError::export("Caminho de destino inválido."))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| AppError::io("criar arquivo temporário de atualização", e))?;
    use std::io::Write;
    temp.write_all(content)
        .map_err(|e| AppError::io("gravar arquivo temporário", e))?;
    temp.flush()
        .map_err(|e| AppError::io("descarregar arquivo temporário", e))?;
    temp.persist(target_path)
        .map_err(|e| AppError::io("publicar arquivo atualizado atomicamente", e.error))?;
    Ok(())
}
