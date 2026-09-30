use std::path::Path;
use std::sync::LazyLock;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionParams, CompletionResponse,
    CompletionTextEdit, Documentation, InsertTextFormat, Position, Range, TextEdit,
};

use crate::completions::{ItemEntry, UnitEntry};
use crate::document::Document;
use crate::state::ServerState;
use crate::utils::position::position_to_offset;

/// Parse unit pairs from embedded data (format: "short = long")
fn parse_unit_pairs(data: &'static str) -> Vec<(&'static str, &'static str)> {
    data.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let mut parts = trimmed.split('=').map(|s| s.trim());
            match (parts.next(), parts.next()) {
                (Some(short), Some(long)) if parts.next().is_none() => Some((short, long)),
                _ => None,
            }
        })
        .collect()
}

/// Parse simple list from embedded data (one item per line)
fn parse_simple_list(data: &'static str) -> Vec<&'static str> {
    data.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Common cooking units (loaded from embedded data/units.txt)
static UNITS: LazyLock<Vec<(&'static str, &'static str)>> =
    LazyLock::new(|| parse_unit_pairs(include_str!("../data/units.txt")));

/// Common time units (loaded from embedded data/time_units.txt)
static TIME_UNITS: LazyLock<Vec<(&'static str, &'static str)>> =
    LazyLock::new(|| parse_unit_pairs(include_str!("../data/time_units.txt")));

/// Common cookware items (loaded from embedded data/cookware.txt)
static COMMON_COOKWARE: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| parse_simple_list(include_str!("../data/cookware.txt")));

/// Common ingredients for suggestions (loaded from embedded data/ingredients.txt)
static COMMON_INGREDIENTS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| parse_simple_list(include_str!("../data/ingredients.txt")));

pub fn get_completions(
    doc: &Document,
    params: &CompletionParams,
    state: &ServerState,
    workspace_root: Option<&Path>,
) -> Option<CompletionResponse> {
    let offset = position_to_offset(params.text_document_position.position, &doc.line_index);
    let text_before = &doc.content[..offset.min(doc.content.len())];

    let context = find_completion_context(text_before)?;

    let items = match context {
        CompletionContext::Ingredient(prefix) => complete_ingredients(&prefix, doc, state),
        CompletionContext::Cookware(prefix) => complete_cookware(&prefix, doc, state),
        CompletionContext::Timer => complete_timer_units(state),
        CompletionContext::Unit(prefix) => complete_units(&prefix, state),
        CompletionContext::Quantity => complete_quantity_snippets(),
        CompletionContext::RecipeReference(prefix) => {
            if let Some(root) = workspace_root {
                // Calculate the range from after '@' to cursor so the client
                // knows exactly what text to replace (. and / break word
                // boundaries, so without an explicit range the client can't
                // match or place completions correctly).
                let after_at_offset = offset - prefix.len();
                let (line, utf8_col) = doc.line_index.line_col(after_at_offset as u32);
                let utf16_col = doc.line_index.utf8_to_utf16_col(line, utf8_col);
                let replace_range = Range {
                    start: Position {
                        line,
                        character: utf16_col,
                    },
                    end: params.text_document_position.position,
                };
                complete_recipe_references(&prefix, root, replace_range)
            } else {
                vec![]
            }
        }
    };

    Some(CompletionResponse::List(CompletionList {
        is_incomplete: false,
        items,
    }))
}

#[derive(Debug)]
enum CompletionContext {
    Ingredient(String),      // After @
    Cookware(String),        // After #
    Timer,                   // After ~
    Unit(String),            // After % or in quantity
    Quantity,                // Inside {} after number
    RecipeReference(String), // After @. (file path reference)
}

fn find_completion_context(text: &str) -> Option<CompletionContext> {
    // Limit backward scan to last 200 characters for performance
    const MAX_SCAN: usize = 200;
    let byte_start = text.len().saturating_sub(MAX_SCAN);
    // Find valid UTF-8 char boundary at or after byte_start
    let scan_start = text.ceil_char_boundary(byte_start);
    let scan_text = &text[scan_start..];

    let chars: Vec<char> = scan_text.chars().collect();
    let len = chars.len();

    // Scan backwards to find context
    for i in (0..len).rev() {
        match chars[i] {
            '@' => {
                let prefix: String = chars[i + 1..].iter().collect();
                // Check we're not inside braces already
                if !prefix.contains('}') {
                    if prefix.contains('{') {
                        // Inside braces - could be quantity context
                        return Some(CompletionContext::Quantity);
                    }
                    let name_prefix = prefix.split('{').next().unwrap_or("").to_string();
                    // Check if this is a recipe/menu file reference (starts with ./ or ../)
                    if name_prefix.starts_with('.') {
                        return Some(CompletionContext::RecipeReference(name_prefix));
                    }
                    return Some(CompletionContext::Ingredient(name_prefix));
                }
                return None;
            }
            '#' => {
                let prefix: String = chars[i + 1..].iter().collect();
                if !prefix.contains('}') {
                    return Some(CompletionContext::Cookware(
                        prefix.split('{').next().unwrap_or("").to_string(),
                    ));
                }
                return None;
            }
            '~' => {
                let rest: String = chars[i + 1..].iter().collect();
                if !rest.contains('}') {
                    return Some(CompletionContext::Timer);
                }
                return None;
            }
            '%' => {
                let prefix: String = chars[i + 1..].iter().collect();
                if !prefix.contains('}') {
                    return Some(CompletionContext::Unit(prefix.trim().to_string()));
                }
                return None;
            }
            '{' => {
                // Check if we're in an ingredient/cookware/timer context
                for j in (0..i).rev() {
                    match chars[j] {
                        '@' | '#' | '~' => {
                            let inside: String = chars[i + 1..].iter().collect();
                            if inside.contains('%') {
                                let after_percent: String =
                                    inside.split('%').next_back().unwrap_or("").to_string();
                                return Some(CompletionContext::Unit(
                                    after_percent.trim().to_string(),
                                ));
                            }
                            return Some(CompletionContext::Quantity);
                        }
                        '\n' | '\r' => break,
                        _ => continue,
                    }
                }
            }
            '\n' | '\r' => break,
            _ => {}
        }
    }
    None
}

fn complete_ingredients(prefix: &str, doc: &Document, state: &ServerState) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let prefix_lower = prefix.to_lowercase();

    // Add existing ingredients from current document (highest priority)
    if let Some(ref result) = doc.parse_result {
        for ingredient in &result.recipe.ingredients {
            let name = &ingredient.name;
            if name.to_lowercase().starts_with(&prefix_lower) {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some("Ingredient (from recipe)".into()),
                    insert_text: Some(format!("{}{{$0}}", name)),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    ..Default::default()
                });
            }
        }
    }

    // Add from other open documents in workspace
    for entry in state.documents.iter() {
        if entry.key() == &doc.uri {
            continue;
        }
        if let Some(ref result) = entry.value().parse_result {
            for ingredient in &result.recipe.ingredients {
                let name = &ingredient.name;
                if name.to_lowercase().starts_with(&prefix_lower)
                    && !items.iter().any(|i| &i.label == name)
                {
                    items.push(CompletionItem {
                        label: name.clone(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail: Some("Ingredient (from workspace)".into()),
                        insert_text: Some(format!("{}{{$0}}", name)),
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        ..Default::default()
                    });
                }
            }
        }
    }

    // Add ingredients from aisle.conf (user's grocery list)
    for aisle_ingredient in state.get_aisle_ingredients() {
        if aisle_ingredient
            .name
            .to_lowercase()
            .starts_with(&prefix_lower)
            && !items.iter().any(|i| i.label == aisle_ingredient.name)
        {
            // Show alias info if this is not the common name
            let detail = if aisle_ingredient.name != aisle_ingredient.common_name {
                format!(
                    "{} (alias for {})",
                    aisle_ingredient.category, aisle_ingredient.common_name
                )
            } else {
                aisle_ingredient.category.clone()
            };

            items.push(CompletionItem {
                label: aisle_ingredient.name.clone(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some(detail),
                documentation: Some(Documentation::String(format!(
                    "From aisle.conf - {}",
                    aisle_ingredient.category
                ))),
                insert_text: Some(format!("{}{{$0}}", aisle_ingredient.name)),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
        }
    }

    // Lowest priority fallback: the embedder's list when one was injected,
    // otherwise the built-in one.
    push_fallback_items(
        &mut items,
        state.custom.ingredients.as_deref(),
        &COMMON_INGREDIENTS,
        &prefix_lower,
        CompletionItemKind::VARIABLE,
        "Common ingredient",
    );

    items
}

/// Push the lowest-priority suggestions for an ingredient or cookware list:
/// the embedder's injected entries when a list was supplied, otherwise the
/// built-in names. `generic_detail` is used for entries carrying neither a
/// detail nor a category.
fn push_fallback_items(
    items: &mut Vec<CompletionItem>,
    injected: Option<&[ItemEntry]>,
    builtin: &[&'static str],
    prefix_lower: &str,
    kind: CompletionItemKind,
    generic_detail: &str,
) {
    match injected {
        Some(entries) => {
            for entry in entries {
                // Promoting `category` into the detail mirrors how aisle.conf
                // entries are presented.
                let detail = entry
                    .detail
                    .as_deref()
                    .or(entry.category.as_deref())
                    .unwrap_or(generic_detail);
                push_fallback_item(items, &entry.name, detail, prefix_lower, kind);
            }
        }
        None => {
            for &name in builtin {
                push_fallback_item(items, name, generic_detail, prefix_lower, kind);
            }
        }
    }
}

fn push_fallback_item(
    items: &mut Vec<CompletionItem>,
    name: &str,
    detail: &str,
    prefix_lower: &str,
    kind: CompletionItemKind,
) {
    if !name.to_lowercase().starts_with(prefix_lower) || items.iter().any(|i| i.label == name) {
        return;
    }
    items.push(CompletionItem {
        label: name.to_string(),
        kind: Some(kind),
        detail: Some(detail.to_string()),
        insert_text: Some(format!("{}{{$0}}", name)),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        ..Default::default()
    });
}

/// Scan a directory recursively for .cook and .menu files
fn scan_recipe_files(root: &Path) -> Vec<(String, &'static str)> {
    let mut files = Vec::new();
    scan_dir_recursive(root, root, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn scan_dir_recursive(root: &Path, dir: &Path, files: &mut Vec<(String, &'static str)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden directories
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if !name.starts_with('.') {
                    scan_dir_recursive(root, &path, files);
                }
            }
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let kind = match ext {
                "cook" => "Recipe",
                "menu" => "Menu",
                _ => continue,
            };
            if let Ok(rel) = path.strip_prefix(root) {
                // Normalize path separators to forward slash and remove extension
                let rel_str = rel.to_string_lossy();
                let without_ext = rel_str
                    .strip_suffix(&format!(".{}", ext))
                    .unwrap_or(&rel_str);
                // Use forward slashes for consistency
                let normalized = without_ext.replace('\\', "/");
                files.push((format!("./{}", normalized), kind));
            }
        }
    }
}

/// Fuzzy match for file paths. Splits on `/` — non-final query segments must
/// prefix-match target segments in order; the final query segment is
/// subsequence-matched against the target filename. Both strings should
/// already be lowercased.
fn fuzzy_match(query: &str, target: &str) -> bool {
    // Just "." means match everything (current directory prefix)
    if query == "." || query == "./" {
        return true;
    }

    let query_segments: Vec<&str> = query.split('/').collect();
    let target_segments: Vec<&str> = target.split('/').collect();

    if query_segments.is_empty() || target_segments.is_empty() {
        return query_segments.is_empty();
    }

    let last_q = query_segments.len() - 1;
    let mut t_idx = 0;

    for (q_idx, q_seg) in query_segments.iter().enumerate() {
        if q_idx == last_q {
            // Last query segment: subsequence match against the filename
            let filename = target_segments.last().unwrap_or(&"");
            return subsequence_match(q_seg, filename);
        }
        // Non-final segments must prefix-match a target segment in order
        let mut found = false;
        while t_idx < target_segments.len() {
            if target_segments[t_idx].starts_with(q_seg) {
                t_idx += 1;
                found = true;
                break;
            }
            t_idx += 1;
        }
        if !found {
            return false;
        }
    }
    true
}

/// Simple subsequence check: all chars of `needle` appear in order in `haystack`.
fn subsequence_match(needle: &str, haystack: &str) -> bool {
    let mut chars = needle.chars().peekable();
    for c in haystack.chars() {
        if chars.peek() == Some(&c) {
            chars.next();
        }
    }
    chars.peek().is_none()
}

fn complete_recipe_references(
    prefix: &str,
    workspace_root: &Path,
    replace_range: Range,
) -> Vec<CompletionItem> {
    let files = scan_recipe_files(workspace_root);
    let prefix_lower = prefix.to_lowercase();

    files
        .into_iter()
        .filter(|(path, _)| fuzzy_match(&prefix_lower, &path.to_lowercase()))
        .map(|(path, kind)| {
            let display_name = path.rsplit('/').next().unwrap_or(&path);
            CompletionItem {
                label: path.clone(),
                kind: Some(CompletionItemKind::FILE),
                detail: Some(format!("{} reference", kind)),
                documentation: Some(Documentation::String(display_name.to_string())),
                filter_text: Some(path.clone()),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range: replace_range,
                    new_text: format!("{}{{$0}}", path),
                })),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            }
        })
        .collect()
}

fn complete_cookware(prefix: &str, doc: &Document, state: &ServerState) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let prefix_lower = prefix.to_lowercase();

    // Add existing cookware from document
    if let Some(ref result) = doc.parse_result {
        for cookware in &result.recipe.cookware {
            let name = &cookware.name;
            if name.to_lowercase().starts_with(&prefix_lower) {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some("Cookware (from recipe)".into()),
                    insert_text: Some(format!("{}{{$0}}", name)),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    ..Default::default()
                });
            }
        }
    }

    push_fallback_items(
        &mut items,
        state.custom.cookware.as_deref(),
        &COMMON_COOKWARE,
        &prefix_lower,
        CompletionItemKind::CLASS,
        "Common cookware",
    );

    items
}

fn complete_timer_units(state: &ServerState) -> Vec<CompletionItem> {
    // `~` takes time units only; measurement units never apply to a timer.
    match state.custom.time_units.as_deref() {
        Some(entries) => entries
            .iter()
            .map(|entry| timer_item(&entry.symbol, entry.name.as_deref()))
            .collect(),
        None => TIME_UNITS
            .iter()
            .map(|(symbol, name)| timer_item(symbol, Some(name)))
            .collect(),
    }
}

fn timer_item(symbol: &str, name: Option<&str>) -> CompletionItem {
    CompletionItem {
        label: symbol.to_string(),
        kind: Some(CompletionItemKind::UNIT),
        detail: name.map(|name| name.to_string()),
        documentation: name.map(|name| Documentation::String(format!("Time unit: {}", name))),
        ..Default::default()
    }
}

fn complete_units(prefix: &str, state: &ServerState) -> Vec<CompletionItem> {
    let prefix_lower = prefix.to_lowercase();
    let mut items = Vec::new();

    push_unit_items(
        &mut items,
        state.custom.units.as_deref(),
        &UNITS,
        &prefix_lower,
        false,
    );
    // Time units are offered here too, so a timer's units stay reachable
    // from inside a quantity.
    push_unit_items(
        &mut items,
        state.custom.time_units.as_deref(),
        &TIME_UNITS,
        &prefix_lower,
        true,
    );

    items
}

/// Push unit suggestions from the embedder's injected list when one was
/// supplied, otherwise from the built-in list. `time` marks the detail as a
/// time unit, distinguishing the two lists that `%` merges.
fn push_unit_items(
    items: &mut Vec<CompletionItem>,
    injected: Option<&[UnitEntry]>,
    builtin: &[(&'static str, &'static str)],
    prefix_lower: &str,
    time: bool,
) {
    match injected {
        Some(entries) => {
            for entry in entries {
                push_unit_item(
                    items,
                    &entry.symbol,
                    entry.name.as_deref(),
                    prefix_lower,
                    time,
                );
            }
        }
        None => {
            for (symbol, name) in builtin {
                push_unit_item(items, symbol, Some(name), prefix_lower, time);
            }
        }
    }
}

fn push_unit_item(
    items: &mut Vec<CompletionItem>,
    symbol: &str,
    name: Option<&str>,
    prefix_lower: &str,
    time: bool,
) {
    if !symbol.to_lowercase().starts_with(prefix_lower) {
        return;
    }
    let detail = match (name, time) {
        (Some(name), true) => Some(format!("{} (time)", name)),
        (Some(name), false) => Some(name.to_string()),
        (None, true) => Some("time".to_string()),
        (None, false) => None,
    };
    items.push(CompletionItem {
        label: symbol.to_string(),
        kind: Some(CompletionItemKind::UNIT),
        detail,
        ..Default::default()
    });
}

fn complete_quantity_snippets() -> Vec<CompletionItem> {
    vec![
        CompletionItem {
            label: "quantity with unit".into(),
            kind: Some(CompletionItemKind::SNIPPET),
            insert_text: Some("${1:amount}%${2:unit}".into()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            detail: Some("Insert quantity with unit".into()),
            ..Default::default()
        },
        CompletionItem {
            label: "quantity only".into(),
            kind: Some(CompletionItemKind::SNIPPET),
            insert_text: Some("${1:amount}".into()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            detail: Some("Insert quantity without unit".into()),
            ..Default::default()
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completions::CustomCompletions;
    use crate::state::AisleConfig;
    use std::fs;
    use tempfile::TempDir;
    use tower_lsp::lsp_types::Url;

    fn state_with(custom: CustomCompletions) -> ServerState {
        let mut state = ServerState::new();
        state.custom = custom;
        state
    }

    fn doc(content: &str) -> Document {
        Document::new(
            Url::parse("file:///test.cook").unwrap(),
            1,
            content.to_string(),
        )
    }

    fn labels(items: &[CompletionItem]) -> Vec<String> {
        items.iter().map(|i| i.label.clone()).collect()
    }

    #[test]
    fn builtins_used_when_nothing_injected() {
        let state = ServerState::new();
        let found = labels(&complete_ingredients("", &doc(""), &state));
        assert!(found.contains(&"salt".to_string()));
        assert_eq!(found.len(), COMMON_INGREDIENTS.len());
    }

    #[test]
    fn injected_ingredients_replace_builtins() {
        let state = state_with(CustomCompletions {
            ingredients: Some(vec![ItemEntry::new("gochujang")]),
            ..Default::default()
        });
        let found = labels(&complete_ingredients("", &doc(""), &state));
        assert!(found.contains(&"gochujang".to_string()));
        // "salt" heads data/ingredients.txt and must not leak through.
        assert!(!found.contains(&"salt".to_string()));
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn empty_injected_list_suppresses_fallbacks() {
        let state = state_with(CustomCompletions {
            ingredients: Some(vec![]),
            ..Default::default()
        });
        assert!(complete_ingredients("", &doc(""), &state).is_empty());
    }

    #[test]
    fn injecting_ingredients_leaves_cookware_builtins() {
        let state = state_with(CustomCompletions {
            ingredients: Some(vec![ItemEntry::new("gochujang")]),
            ..Default::default()
        });
        let found = labels(&complete_cookware("", &doc(""), &state));
        assert!(found.contains(&"pot".to_string()));
        assert_eq!(found.len(), COMMON_COOKWARE.len());
    }

    #[test]
    fn document_and_aisle_ingredients_survive_injection() {
        let state = state_with(CustomCompletions {
            ingredients: Some(vec![ItemEntry::new("gochujang")]),
            ..Default::default()
        });
        *state.aisle_config.write().unwrap() = AisleConfig::parse("[produce]\nkohlrabi\n");

        let found = labels(&complete_ingredients(
            "",
            &doc("Mix @tamarind{1%tbsp}."),
            &state,
        ));
        assert!(
            found.contains(&"tamarind".to_string()),
            "document: {found:?}"
        );
        assert!(found.contains(&"kohlrabi".to_string()), "aisle: {found:?}");
        assert!(
            found.contains(&"gochujang".to_string()),
            "injected: {found:?}"
        );
    }

    #[test]
    fn injected_items_respect_the_prefix_filter() {
        let state = state_with(CustomCompletions {
            ingredients: Some(vec![ItemEntry::new("gochujang"), ItemEntry::new("miso")]),
            ..Default::default()
        });
        assert_eq!(
            labels(&complete_ingredients("go", &doc(""), &state)),
            vec!["gochujang".to_string()]
        );
    }

    #[test]
    fn injected_units_do_not_reach_timer_completions() {
        let state = state_with(CustomCompletions {
            units: Some(vec![UnitEntry::new("shaku", "shaku")]),
            ..Default::default()
        });

        let timer = labels(&complete_timer_units(&state));
        assert!(!timer.contains(&"shaku".to_string()));
        assert!(timer.contains(&"min".to_string()), "built-in time units");

        // `%` merges both lists: injected measurement units, built-in time units.
        let units = labels(&complete_units("", &state));
        assert!(units.contains(&"shaku".to_string()));
        assert!(units.contains(&"min".to_string()));
        assert!(!units.contains(&"kg".to_string()), "builtin units replaced");
    }

    #[test]
    fn injected_time_units_replace_timer_and_merge_into_units() {
        let state = state_with(CustomCompletions {
            time_units: Some(vec![UnitEntry::new("ks", "kiloseconds")]),
            ..Default::default()
        });

        assert_eq!(
            labels(&complete_timer_units(&state)),
            vec!["ks".to_string()]
        );

        let units = labels(&complete_units("", &state));
        assert!(units.contains(&"ks".to_string()));
        assert!(
            units.contains(&"kg".to_string()),
            "measurement units intact"
        );
        assert!(!units.contains(&"min".to_string()));
    }

    #[test]
    fn detail_falls_back_through_category_to_generic() {
        let state = state_with(CustomCompletions {
            ingredients: Some(vec![
                ItemEntry {
                    name: "aaa".into(),
                    detail: Some("explicit".into()),
                    category: Some("ignored".into()),
                },
                ItemEntry {
                    name: "bbb".into(),
                    detail: None,
                    category: Some("condiments".into()),
                },
                ItemEntry::new("ccc"),
            ]),
            ..Default::default()
        });

        let items = complete_ingredients("", &doc(""), &state);
        let detail = |label: &str| {
            items
                .iter()
                .find(|i| i.label == label)
                .unwrap()
                .detail
                .clone()
                .unwrap()
        };
        assert_eq!(detail("aaa"), "explicit");
        assert_eq!(detail("bbb"), "condiments");
        assert_eq!(detail("ccc"), "Common ingredient");
    }

    #[test]
    fn unit_detail_marks_time_and_tolerates_missing_names() {
        let state = state_with(CustomCompletions {
            units: Some(vec![UnitEntry::symbol_only("sho")]),
            time_units: Some(vec![
                UnitEntry::new("ks", "kiloseconds"),
                UnitEntry::symbol_only("ms"),
            ]),
            ..Default::default()
        });

        let items = complete_units("", &state);
        let find = |label: &str| items.iter().find(|i| i.label == label).unwrap();
        assert_eq!(find("sho").detail, None);
        assert_eq!(find("ks").detail.as_deref(), Some("kiloseconds (time)"));
        assert_eq!(find("ms").detail.as_deref(), Some("time"));
    }

    #[test]
    fn test_context_recipe_reference_dot() {
        let ctx = find_completion_context("Pour over with @.").unwrap();
        assert!(matches!(ctx, CompletionContext::RecipeReference(ref p) if p == "."));
    }

    #[test]
    fn test_context_recipe_reference_dot_slash() {
        let ctx = find_completion_context("Pour over with @./").unwrap();
        assert!(matches!(ctx, CompletionContext::RecipeReference(ref p) if p == "./"));
    }

    #[test]
    fn test_context_recipe_reference_path() {
        let ctx = find_completion_context("Pour over with @./sauces/Hol").unwrap();
        assert!(matches!(ctx, CompletionContext::RecipeReference(ref p) if p == "./sauces/Hol"));
    }

    #[test]
    fn test_context_recipe_reference_parent() {
        let ctx = find_completion_context("@../other/Recipe").unwrap();
        assert!(matches!(ctx, CompletionContext::RecipeReference(ref p) if p == "../other/Recipe"));
    }

    #[test]
    fn test_context_recipe_reference_with_brace_is_quantity() {
        let ctx = find_completion_context("@./sauces/Hollandaise{").unwrap();
        assert!(matches!(ctx, CompletionContext::Quantity));
    }

    #[test]
    fn test_context_regular_ingredient_unchanged() {
        let ctx = find_completion_context("@sal").unwrap();
        assert!(matches!(ctx, CompletionContext::Ingredient(ref p) if p == "sal"));
    }

    #[test]
    fn test_scan_recipe_files() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Create directory structure
        fs::create_dir_all(root.join("sauces")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();

        // Create recipe files
        fs::write(root.join("Pancakes.cook"), "").unwrap();
        fs::write(root.join("sauces/Hollandaise.cook"), "").unwrap();
        fs::write(root.join("sauces/Bechamel.cook"), "").unwrap();
        fs::write(root.join("WeeklyMenu.menu"), "").unwrap();
        fs::write(root.join("notes.txt"), "").unwrap();
        fs::write(root.join(".hidden/Secret.cook"), "").unwrap();

        let files = scan_recipe_files(root);
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();

        assert!(paths.contains(&"./Pancakes"));
        assert!(paths.contains(&"./sauces/Hollandaise"));
        assert!(paths.contains(&"./sauces/Bechamel"));
        assert!(paths.contains(&"./WeeklyMenu"));

        // Should not include non-recipe files
        assert!(!paths.iter().any(|p| p.contains("notes")));
        // Should not include hidden directory files
        assert!(!paths.iter().any(|p| p.contains("Secret")));

        // Check kinds
        let menu = files.iter().find(|(p, _)| p == "./WeeklyMenu").unwrap();
        assert_eq!(menu.1, "Menu");

        let recipe = files.iter().find(|(p, _)| p == "./Pancakes").unwrap();
        assert_eq!(recipe.1, "Recipe");
    }

    #[test]
    fn test_complete_recipe_references_filtering() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("sauces")).unwrap();
        fs::write(root.join("sauces/Hollandaise.cook"), "").unwrap();
        fs::write(root.join("sauces/Bechamel.cook"), "").unwrap();
        fs::write(root.join("Pancakes.cook"), "").unwrap();

        // Dummy range for tests
        let range = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 0,
            },
        };

        // Filter by directory + partial filename (fuzzy on filename)
        let items = complete_recipe_references("./sauces/Hol", root, range);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "./sauces/Hollandaise");
        // text_edit should contain the snippet
        match &items[0].text_edit {
            Some(CompletionTextEdit::Edit(edit)) => {
                assert_eq!(edit.new_text, "./sauces/Hollandaise{$0}");
            }
            _ => panic!("Expected text_edit"),
        }

        // All sauces
        let items = complete_recipe_references("./sauces/", root, range);
        assert_eq!(items.len(), 2);

        // Everything
        let items = complete_recipe_references("./", root, range);
        assert_eq!(items.len(), 3);

        // Just dot
        let items = complete_recipe_references(".", root, range);
        assert_eq!(items.len(), 3);

        // Fuzzy match across path segments
        let items = complete_recipe_references("./hol", root, range);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "./sauces/Hollandaise");

        // Fuzzy match - short query
        let items = complete_recipe_references("./pan", root, range);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "./Pancakes");
    }

    #[test]
    fn test_fuzzy_match() {
        // Fuzzy on filename (last segment of target)
        assert!(fuzzy_match("./hol", "./sauces/hollandaise"));
        assert!(fuzzy_match("./pan", "./pancakes"));
        assert!(fuzzy_match("./", "./anything"));
        assert!(fuzzy_match(".", "./anything"));
        // No match
        assert!(!fuzzy_match("./xyz", "./pancakes"));
        // Directory prefix + fuzzy filename
        assert!(fuzzy_match("./sauces/hol", "./sauces/hollandaise"));
        assert!(fuzzy_match("./sauces/b", "./sauces/bechamel"));
        // Wrong directory excludes results
        assert!(!fuzzy_match("./sauces/p", "./pancakes"));
        // Subsequence within filename
        assert!(fuzzy_match("./bml", "./sauces/bechamel"));
        assert!(!fuzzy_match("./zz", "./sauces/bechamel"));
    }
}
