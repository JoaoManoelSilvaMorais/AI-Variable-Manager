use std::collections::HashMap;

use strsim::jaro_winkler;

use crate::domain::asset::{
    Asset, MatchCandidate, REASON_AMBIGUOUS_TIE, REASON_FUZZY_STEM,
};
use crate::domain::project::ProjectSnapshot;
use crate::domain::record::{MatchStatus, RecordValue};
use crate::domain::variable::VariableKind;
use crate::shared::text_norm::normalize_match_key;

const FUZZY_THRESHOLD: f64 = 0.86;
const MAX_FUZZY_CANDIDATES: usize = 5;

pub struct AutoMatchOutput {
    pub updated_values: Vec<RecordValue>,
    pub candidates: Vec<MatchCandidate>,
}

/// Runs Method A deterministic matching across all `LinkedFile` variables in `snap`,
/// never mutating any `text_value` and preserving existing `Manual` links.
pub fn run_auto_match_for_snapshot(snap: &ProjectSnapshot) -> AutoMatchOutput {
    let available_assets: Vec<&Asset> = snap.assets.iter().filter(|a| a.exists_on_disk).collect();

    let mut by_normalized_stem: HashMap<&str, Vec<&Asset>> = HashMap::new();
    for asset in &available_assets {
        by_normalized_stem
            .entry(asset.normalized_stem.as_str())
            .or_default()
            .push(*asset);
    }

    let val_map: HashMap<(&str, &str), &RecordValue> = snap
        .values
        .iter()
        .map(|v| ((v.record_id.as_str(), v.variable_id.as_str()), v))
        .collect();

    let text_vars: Vec<&str> = snap
        .variables
        .iter()
        .filter(|v| v.kind == VariableKind::Text)
        .map(|v| v.id.as_str())
        .collect();

    let linked_vars: Vec<&str> = snap
        .variables
        .iter()
        .filter(|v| v.kind == VariableKind::LinkedFile)
        .map(|v| v.id.as_str())
        .collect();

    let mut updated_values = Vec::new();
    let mut candidates = Vec::new();

    for var_id in linked_vars {
        for rec in &snap.records {
            let existing = val_map.get(&(rec.id.as_str(), var_id)).copied();
            let raw_text = existing.map(|v| v.text_value.as_str()).unwrap_or("");

            if let Some(prev) = existing
                && prev.match_status == MatchStatus::Manual
                && prev.asset_id.is_some()
            {
                continue;
            }

            let lookup_key = resolve_lookup_key(
                raw_text,
                &rec.id,
                &text_vars,
                &val_map,
                &by_normalized_stem,
            );

            if lookup_key.is_empty() || available_assets.is_empty() {
                updated_values.push(RecordValue {
                    record_id: rec.id.clone(),
                    variable_id: var_id.to_string(),
                    text_value: raw_text.to_string(),
                    asset_id: None,
                    match_status: MatchStatus::Unmatched,
                    match_confidence: None,
                });
                continue;
            }

            if let Some(exact_matches) = by_normalized_stem.get(lookup_key.as_str()) {
                if exact_matches.len() == 1 {
                    let chosen = exact_matches[0];
                    updated_values.push(RecordValue {
                        record_id: rec.id.clone(),
                        variable_id: var_id.to_string(),
                        text_value: raw_text.to_string(),
                        asset_id: Some(chosen.id.clone()),
                        match_status: MatchStatus::AutoExact,
                        match_confidence: Some(1.0),
                    });
                } else {
                    let mut sorted_ties = exact_matches.clone();
                    sorted_ties.sort_by(|a, b| a.file_name.cmp(&b.file_name));
                    for asset in sorted_ties {
                        candidates.push(MatchCandidate {
                            record_id: rec.id.clone(),
                            variable_id: var_id.to_string(),
                            asset_id: asset.id.clone(),
                            score: 1.0,
                            reason: REASON_AMBIGUOUS_TIE.to_string(),
                        });
                    }
                    updated_values.push(RecordValue {
                        record_id: rec.id.clone(),
                        variable_id: var_id.to_string(),
                        text_value: raw_text.to_string(),
                        asset_id: None,
                        match_status: MatchStatus::Ambiguous,
                        match_confidence: Some(1.0),
                    });
                }
                continue;
            }

            // Fuzzy fallback via Jaro-Winkler (>= 0.86)
            let fuzzy = collect_fuzzy_candidates(&lookup_key, &available_assets);
            if let Some((_, best_score)) = fuzzy.first() {
                let best_score = *best_score;
                for (asset, score) in fuzzy {
                    candidates.push(MatchCandidate {
                        record_id: rec.id.clone(),
                        variable_id: var_id.to_string(),
                        asset_id: asset.id.clone(),
                        score,
                        reason: REASON_FUZZY_STEM.to_string(),
                    });
                }
                updated_values.push(RecordValue {
                    record_id: rec.id.clone(),
                    variable_id: var_id.to_string(),
                    text_value: raw_text.to_string(),
                    asset_id: None,
                    match_status: MatchStatus::AutoSuggested,
                    match_confidence: Some(best_score),
                });
            } else {
                updated_values.push(RecordValue {
                    record_id: rec.id.clone(),
                    variable_id: var_id.to_string(),
                    text_value: raw_text.to_string(),
                    asset_id: None,
                    match_status: MatchStatus::Unmatched,
                    match_confidence: None,
                });
            }
        }
    }

    AutoMatchOutput {
        updated_values,
        candidates,
    }
}

fn resolve_lookup_key(
    linked_cell_text: &str,
    record_id: &str,
    text_var_ids: &[&str],
    val_map: &HashMap<(&str, &str), &RecordValue>,
    by_normalized_stem: &HashMap<&str, Vec<&Asset>>,
) -> String {
    let direct = normalize_match_key(linked_cell_text);
    if !direct.is_empty() {
        return direct;
    }

    let mut fallback_keys = Vec::new();
    for &tv_id in text_var_ids {
        if let Some(rv) = val_map.get(&(record_id, tv_id)) {
            let key = normalize_match_key(&rv.text_value);
            if !key.is_empty() {
                if by_normalized_stem.contains_key(key.as_str()) {
                    return key;
                }
                fallback_keys.push(key);
            }
        }
    }

    fallback_keys.into_iter().next().unwrap_or_default()
}

fn collect_fuzzy_candidates<'a>(
    lookup_key: &str,
    assets: &[&'a Asset],
) -> Vec<(&'a Asset, f64)> {
    if lookup_key.chars().count() < 3 {
        return Vec::new();
    }

    let mut scored: Vec<(&'a Asset, f64)> = assets
        .iter()
        .filter(|asset| asset.normalized_stem.chars().count() >= 3)
        .filter_map(|&asset| {
            let score = jaro_winkler(lookup_key, &asset.normalized_stem);
            if score >= FUZZY_THRESHOLD {
                Some((asset, score))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.file_name.cmp(&b.0.file_name))
    });
    scored.truncate(MAX_FUZZY_CANDIDATES);
    scored
}
