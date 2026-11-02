//! Per-harness config writers: where a tag lands *inside* one harness config.
//!
//! A writer is selected by a row's `writer` key, so adding a harness is a data
//! change plus at most one small function here — never a change to the overlay
//! machinery. Writers only touch the document they are handed; where the
//! document lives (a per-session overlay, a test temp dir) is not their concern,
//! which is what makes them cheap to test with a fixture file.
//!
//! A delivery the harness cannot express is returned as a skip with a reason,
//! never written. Nothing here reads the user's real config.

use serde_json::Value;

use crate::overlay::Resolved;

/// The document a writer works on. JSON and TOML harnesses are both first-class:
/// TOML keeps its comments, because a user's `config.toml` has them.
pub enum Doc {
    Json(Value),
    Toml(toml_edit::DocumentMut),
}

impl Doc {
    /// Parse the user's file into the shape its writer expects.
    pub fn parse(format: &str, raw: Option<&str>) -> Result<Self, String> {
        match format {
            "toml" => Ok(Doc::Toml(
                raw.unwrap_or_default()
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(|err| format!("not valid TOML: {err}"))?,
            )),
            _ => match raw {
                Some(raw) => serde_json::from_str(raw)
                    .map(Doc::Json)
                    .map_err(|err| format!("not valid JSON: {err}")),
                None => Ok(Doc::Json(Value::Object(serde_json::Map::new()))),
            },
        }
    }

    /// The file's text, newline-terminated.
    pub fn render(&self) -> Result<String, String> {
        match self {
            Doc::Json(value) => serde_json::to_string_pretty(value)
                .map(|text| format!("{text}\n"))
                .map_err(|err| format!("serialize failed: {err}")),
            Doc::Toml(doc) => Ok(doc.to_string()),
        }
    }
}

/// Put every target into `doc`. Returns the skips as `(target, reason)`.
pub fn apply(
    writer: &str,
    doc: &mut Doc,
    targets: &[Resolved],
    source: &std::path::Path,
) -> Result<Vec<(String, String)>, String> {
    match (writer, doc) {
        ("pi_models", Doc::Json(value)) => pi_models(value, targets, source),
        ("opencode_json", Doc::Json(value)) => opencode_json(value, targets),
        ("codex_toml", Doc::Toml(doc)) => codex_toml(doc, targets),
        (other, _) => Err(format!(
            "writer {other:?} is not implemented for this config format; add it to src/writer.rs"
        )),
    }
}

/// pi `models.json`: `providers.<id>.headers` plus `models[].samplingParams`.
///
/// The provider must already carry a non-empty `models` list (host pi-proxy
/// discards a headers-only entry), and `samplingParams` only exists per model —
/// a provider-level object is dropped by pi's schema.
fn pi_models(
    doc: &mut Value,
    targets: &[Resolved],
    source: &std::path::Path,
) -> Result<Vec<(String, String)>, String> {
    let mut skipped = Vec::new();
    let Some(providers) = doc.get_mut("providers").and_then(Value::as_object_mut) else {
        return Ok(named_skip(targets, "the config has no providers object"));
    };
    let mut names: Vec<String> = providers.keys().cloned().collect();
    names.sort();
    let target = names
        .iter()
        .find(|name| {
            providers
                .get(*name)
                .and_then(|entry| entry.get("models"))
                .and_then(Value::as_array)
                .is_some_and(|models| !models.is_empty())
        })
        .cloned();
    let Some(provider_id) = target else {
        return Ok(named_skip(
            targets,
            "the config has no provider entry with a non-empty models list",
        ));
    };
    let Some(provider) = providers
        .get_mut(&provider_id)
        .and_then(Value::as_object_mut)
    else {
        return Err(format!("provider {provider_id} is not an object"));
    };

    for item in targets {
        match item.kind.as_str() {
            "header" => {
                let headers = provider
                    .entry("headers")
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                let Some(headers) = headers.as_object_mut() else {
                    skipped.push((
                        item.name.clone(),
                        "provider headers is not an object".into(),
                    ));
                    continue;
                };
                headers.insert(item.name.clone(), Value::String(item.value.clone()));
            }
            "body" => {
                let Some(models) = provider
                    .get_mut("models")
                    .and_then(Value::as_array_mut)
                    .filter(|models| !models.is_empty())
                else {
                    skipped.push((
                        item.name.clone(),
                        "no model entry to carry a body field".into(),
                    ));
                    continue;
                };
                for model in models {
                    let Some(model) = model.as_object_mut() else {
                        skipped.push((
                            item.name.clone(),
                            "a models[] entry is not an object".into(),
                        ));
                        break;
                    };
                    let params = model
                        .entry("samplingParams")
                        .or_insert_with(|| Value::Object(serde_json::Map::new()));
                    let Some(params) = params.as_object_mut() else {
                        skipped.push((
                            item.name.clone(),
                            "a model samplingParams is not an object".into(),
                        ));
                        break;
                    };
                    params.insert(item.name.clone(), json_value(&item.value));
                }
            }
            other => skipped.push((
                item.name.clone(),
                format!("pi cannot deliver a {other} value"),
            )),
        }
    }
    let _ = source;
    Ok(skipped)
}

/// opencode `opencode.json`: `provider.<id>.options.headers`.
fn opencode_json(doc: &mut Value, targets: &[Resolved]) -> Result<Vec<(String, String)>, String> {
    let mut skipped = Vec::new();
    let Some(providers) = doc.get_mut("provider").and_then(Value::as_object_mut) else {
        return Ok(named_skip(targets, "the config has no provider object"));
    };
    let provider_id = {
        let mut names: Vec<String> = providers.keys().cloned().collect();
        names.sort();
        // Prefer a provider that already names an endpoint, else the first one.
        names
            .iter()
            .find(|name| {
                providers
                    .get(*name)
                    .and_then(|entry| entry.get("options"))
                    .and_then(|options| options.get("baseURL"))
                    .is_some()
            })
            .or_else(|| names.first())
            .cloned()
    };
    let Some(provider_id) = provider_id else {
        return Ok(named_skip(targets, "the config has no provider entries"));
    };
    let Some(provider) = providers
        .get_mut(&provider_id)
        .and_then(Value::as_object_mut)
    else {
        return Err(format!("provider {provider_id} is not an object"));
    };
    let options = provider
        .entry("options")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(options) = options.as_object_mut() else {
        return Ok(named_skip(targets, "provider options is not an object"));
    };
    for item in targets {
        // `options.headers` and `options.queryParams` both reach the AI SDK
        // provider unchanged; there is no body knob it forwards.
        let key = match item.kind.as_str() {
            "header" => "headers",
            "query" => "queryParams",
            other => {
                skipped.push((
                    item.name.clone(),
                    format!("opencode cannot carry a {other} value"),
                ));
                continue;
            }
        };
        let map = options
            .entry(key)
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        let Some(map) = map.as_object_mut() else {
            skipped.push((item.name.clone(), format!("options.{key} is not an object")));
            continue;
        };
        map.insert(item.name.clone(), Value::String(item.value.clone()));
    }
    Ok(skipped)
}

/// codex `config.toml`: static `http_headers` on the selected model provider.
///
/// The copy is session-owned, so a literal is right here: codex's own
/// `env_http_headers` would read the *name* of a variable, and this file is the
/// place where that indirection stops paying for itself. Comments survive
/// because `toml_edit` edits the document instead of re-serialising a tree.
fn codex_toml(
    doc: &mut toml_edit::DocumentMut,
    targets: &[Resolved],
) -> Result<Vec<(String, String)>, String> {
    let mut skipped = Vec::new();
    let provider = {
        let configured = doc
            .get("model_provider")
            .and_then(|item| item.as_str())
            .map(str::to_string);
        match configured.filter(|name| provider_table(doc, name).is_some()) {
            Some(name) => name,
            None => {
                let mut names: Vec<String> = doc
                    .get("model_providers")
                    .and_then(|item| item.as_table_like())
                    .map(|table| table.iter().map(|(name, _)| name.to_string()).collect())
                    .unwrap_or_default();
                names.sort();
                match names.first() {
                    Some(name) => name.clone(),
                    None => return Ok(named_skip(targets, "the config names no model provider")),
                }
            }
        }
    };
    for item in targets {
        // codex appends the endpoint path itself, so only these two tables can
        // carry a value: `query_params` becomes `?name=value` on the request.
        let key = match item.kind.as_str() {
            "header" => "http_headers",
            "query" => "query_params",
            other => {
                skipped.push((
                    item.name.clone(),
                    format!("codex cannot carry a {other} value"),
                ));
                continue;
            }
        };
        let Some(provider) = provider_table(doc, &provider) else {
            skipped.push((
                item.name.clone(),
                format!("provider {provider} is not a table"),
            ));
            continue;
        };
        let table = provider
            .entry(key)
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        let Some(table) = table.as_table_like_mut() else {
            skipped.push((item.name.clone(), format!("{key} is not a table")));
            continue;
        };
        table.insert(&item.name, toml_edit::value(item.value.clone()));
    }
    Ok(skipped)
}

fn provider_table<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    name: &str,
) -> Option<&'a mut dyn toml_edit::TableLike> {
    doc.get_mut("model_providers")?
        .as_table_like_mut()?
        .get_mut(name)?
        .as_table_like_mut()
}

fn named_skip(targets: &[Resolved], reason: &str) -> Vec<(String, String)> {
    targets
        .iter()
        .map(|item| (item.name.clone(), reason.to_string()))
        .collect()
}

/// A value that parses as JSON goes in as JSON; anything else stays a string.
pub fn json_value(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) if !raw.trim().is_empty() => value,
        _ => Value::String(raw.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn json_doc(value: Value) -> Doc {
        Doc::Json(value)
    }

    fn target(kind: &str, name: &str, value: &str) -> Resolved {
        Resolved {
            kind: kind.to_string(),
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn pi_writes_headers_and_puts_body_fields_in_every_model() {
        let mut doc =
            json_doc(json!({"providers": {"acme": {"models": [{"id": "a"}, {"id": "b"}]}}}));
        let skipped = apply(
            "pi_models",
            &mut doc,
            &[
                target("header", "X-AoE-Tags", "aoe,team:core"),
                target("body", "metadata", "{\"tags\":[\"aoe\"]}"),
            ],
            std::path::Path::new("/nonexistent"),
        )
        .expect("writer runs");
        assert!(skipped.is_empty());
        let Doc::Json(doc) = &doc else {
            panic!("json doc")
        };
        let provider = &doc["providers"]["acme"];
        assert_eq!(provider["headers"]["X-AoE-Tags"], "aoe,team:core");
        assert_eq!(
            provider["models"][0]["samplingParams"]["metadata"]["tags"][0],
            "aoe"
        );
        assert_eq!(
            provider["models"][1]["samplingParams"]["metadata"]["tags"][0],
            "aoe"
        );
    }

    #[test]
    fn pi_refuses_a_query_delivery_instead_of_writing_it() {
        let mut doc = json_doc(json!({"providers": {"acme": {"models": [{"id": "a"}]}}}));
        let skipped = apply(
            "pi_models",
            &mut doc,
            &[target("query", "tags", "aoe")],
            std::path::Path::new("/nonexistent"),
        )
        .expect("writer runs");
        assert_eq!(skipped.len(), 1);
        assert!(!skipped[0].1.is_empty());
        let Doc::Json(doc) = &doc else {
            panic!("json doc")
        };
        assert!(doc["providers"]["acme"].get("baseUrl").is_none());
    }

    #[test]
    fn opencode_writes_headers_under_options() {
        let mut doc = json_doc(
            json!({"provider": {"acme": {"options": {"baseURL": "http://localhost:8899/v1"}}}}),
        );
        let skipped = apply(
            "opencode_json",
            &mut doc,
            &[target("header", "X-AoE-Tags", "aoe")],
            std::path::Path::new("/nonexistent"),
        )
        .expect("writer runs");
        assert!(skipped.is_empty());
        let Doc::Json(doc) = &doc else {
            panic!("json doc")
        };
        assert_eq!(
            doc["provider"]["acme"]["options"]["headers"]["X-AoE-Tags"],
            "aoe"
        );
    }

    #[test]
    fn codex_query_params_become_a_query_parameter() {
        let raw =
            "model_provider = \"acme\"\n\n[model_providers.acme]\nbase_url = \"http://h/v1\"\n";
        let mut doc = Doc::parse("toml", Some(raw)).expect("parses");
        let skipped = apply(
            "codex_toml",
            &mut doc,
            &[target("query", "tags", "aoe")],
            std::path::Path::new("/nonexistent"),
        )
        .expect("writer runs");
        assert!(skipped.is_empty(), "{skipped:?}");
        let text = doc.render().unwrap();
        assert!(text.contains("query_params"), "{text}");
        assert!(text.contains("tags = \"aoe\""), "{text}");
        assert!(!text.contains("http_headers"), "{text}");
    }

    #[test]
    fn opencode_query_params_are_written_next_to_the_headers() {
        let mut doc = json_doc(json!({"provider": {"acme": {"options": {}}}}));
        let skipped = apply(
            "opencode_json",
            &mut doc,
            &[
                target("header", "X-AoE-Tags", "aoe"),
                target("query", "tags", "aoe"),
            ],
            std::path::Path::new("/nonexistent"),
        )
        .expect("writer runs");
        assert!(skipped.is_empty(), "{skipped:?}");
        let Doc::Json(doc) = &doc else {
            panic!("json doc")
        };
        assert_eq!(
            doc["provider"]["acme"]["options"]["headers"]["X-AoE-Tags"],
            "aoe"
        );
        assert_eq!(
            doc["provider"]["acme"]["options"]["queryParams"]["tags"],
            "aoe"
        );
    }

    #[test]
    fn codex_writes_static_headers_and_keeps_the_comments_around_them() {
        let raw = "# my provider, hand written\nmodel_provider = \"acme\"\n\n[model_providers.acme]\nname = \"acme\"  # keep me\nbase_url = \"http://h/v1\"\n";
        let mut doc = Doc::parse("toml", Some(raw)).expect("parses");
        let skipped = apply(
            "codex_toml",
            &mut doc,
            &[target("header", "X-AoE-Tags", "aoe")],
            std::path::Path::new("/nonexistent"),
        )
        .expect("writer runs");
        assert!(skipped.is_empty());
        let text = doc.render().unwrap();
        assert!(text.contains("# my provider, hand written"), "{text}");
        assert!(text.contains("# keep me"), "{text}");
        assert!(text.contains("X-AoE-Tags = \"aoe\""), "{text}");
    }

    #[test]
    fn a_json_literal_becomes_a_json_value_and_text_stays_text() {
        assert_eq!(json_value("{\"a\":1}")["a"], 1);
        assert_eq!(json_value("plain"), "plain");
    }
}
