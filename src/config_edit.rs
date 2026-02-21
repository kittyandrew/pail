use std::collections::HashSet;

use anyhow::{Context, Result};
use toml_edit::{Array, DocumentMut, Formatted, Item, Table, Value};

/// A new source to add to the config file.
pub struct NewSource {
    pub name: String,
    pub source_type: String,
    pub tg_username: Option<String>,
    pub tg_id: Option<i64>,
    pub tg_folder_name: Option<String>,
    pub description: Option<String>,
}

/// Parse a TOML config file into a document-preserving representation.
pub fn parse_document(content: &str) -> Result<DocumentMut> {
    content.parse::<DocumentMut>().context("parsing TOML document")
}

/// Get names of all sources.
pub fn get_all_source_names(doc: &DocumentMut) -> Vec<String> {
    get_sources_matching(doc, |_| true)
}

/// Append a new `[[source]]` table to the document.
pub fn add_source(doc: &mut DocumentMut, source: &NewSource) {
    // Build the new source table
    let mut table = Table::new();
    table.set_implicit(true);

    table.insert("name", toml_edit::value(&source.name));
    table.insert("type", toml_edit::value(&source.source_type));

    if let Some(ref username) = source.tg_username {
        table.insert("tg_username", toml_edit::value(username));
    }

    if let Some(tg_id) = source.tg_id {
        table.insert("tg_id", toml_edit::value(tg_id));
    }

    if let Some(ref folder_name) = source.tg_folder_name {
        table.insert("tg_folder_name", toml_edit::value(folder_name));
    }

    if let Some(ref description) = source.description {
        table.insert("description", toml_edit::value(description));
    }

    // Get or create the [[source]] array of tables
    let sources = doc
        .entry("source")
        .or_insert_with(|| Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .expect("source should be array of tables");

    sources.push(table);
}

/// Remove a source by name. Returns true if found and removed.
pub fn remove_source(doc: &mut DocumentMut, source_name: &str) -> bool {
    let Some(sources) = doc.get_mut("source").and_then(|v| v.as_array_of_tables_mut()) else {
        return false;
    };

    let mut index_to_remove = None;
    for (i, source) in sources.iter().enumerate() {
        if source.get("name").and_then(|v| v.as_str()) == Some(source_name) {
            index_to_remove = Some(i);
            break;
        }
    }

    if let Some(i) = index_to_remove {
        sources.remove(i);
        true
    } else {
        false
    }
}

/// Render the document back to a TOML string.
pub fn render(doc: &DocumentMut) -> String {
    doc.to_string()
}

/// Get source names matching a predicate on source type.
fn get_sources_matching(doc: &DocumentMut, predicate: impl Fn(&str) -> bool) -> Vec<String> {
    let mut names = Vec::new();

    let Some(sources) = doc.get("source").and_then(|v| v.as_array_of_tables()) else {
        return names;
    };

    for source in sources.iter() {
        let name = source.get("name").and_then(|v| v.as_str());
        let source_type = source.get("type").and_then(|v| v.as_str());

        if let (Some(name), Some(stype)) = (name, source_type)
            && predicate(stype)
        {
            names.push(name.to_string());
        }
    }

    names
}

/// Metadata about a Telegram source in the config, used for matching dialogs to existing sources.
pub struct TgSourceInfo {
    pub name: String,
    pub tg_id: Option<i64>,
    pub tg_username: Option<String>,
    pub tg_folder_name: Option<String>,
}

/// List all output channel names.
pub fn get_output_channel_names(doc: &DocumentMut) -> Vec<String> {
    let Some(channels) = doc.get("output_channel").and_then(|v| v.as_array_of_tables()) else {
        return Vec::new();
    };

    channels
        .iter()
        .filter_map(|ch| ch.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect()
}

/// Get a channel's `sources` array by channel name.
pub fn get_channel_sources(doc: &DocumentMut, channel_name: &str) -> Vec<String> {
    let Some(channels) = doc.get("output_channel").and_then(|v| v.as_array_of_tables()) else {
        return Vec::new();
    };

    for channel in channels.iter() {
        if channel.get("name").and_then(|v| v.as_str()) == Some(channel_name) {
            if let Some(sources) = channel.get("sources").and_then(|v| v.as_array()) {
                return sources
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
            }
            return Vec::new();
        }
    }

    Vec::new()
}

/// Replace a channel's `sources` array. Returns true if the channel was found.
pub fn set_channel_sources(doc: &mut DocumentMut, channel_name: &str, sources: &[String]) -> bool {
    let Some(channels) = doc.get_mut("output_channel").and_then(|v| v.as_array_of_tables_mut()) else {
        return false;
    };

    for channel in channels.iter_mut() {
        if channel.get("name").and_then(|v| v.as_str()) == Some(channel_name) {
            let mut arr = Array::new();
            for name in sources {
                arr.push(Value::String(Formatted::new(name.clone())));
            }
            channel.insert("sources", toml_edit::value(arr));
            return true;
        }
    }

    false
}

/// Get detailed metadata for all Telegram sources (channels, groups, and folders).
pub fn get_tg_sources_detailed(doc: &DocumentMut) -> Vec<TgSourceInfo> {
    let Some(sources) = doc.get("source").and_then(|v| v.as_array_of_tables()) else {
        return Vec::new();
    };

    sources
        .iter()
        .filter_map(|source| {
            let name = source.get("name").and_then(|v| v.as_str())?.to_string();
            let source_type = source.get("type").and_then(|v| v.as_str())?;
            if !source_type.starts_with("telegram_") {
                return None;
            }

            let tg_id = source.get("tg_id").and_then(|v| v.as_integer());
            let tg_username = source
                .get("tg_username")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let tg_folder_name = source
                .get("tg_folder_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            Some(TgSourceInfo {
                name,
                tg_id,
                tg_username,
                tg_folder_name,
            })
        })
        .collect()
}

/// Get all source names referenced by any output channel.
pub fn get_all_source_names_in_any_channel(doc: &DocumentMut) -> HashSet<String> {
    let mut result = HashSet::new();

    let Some(channels) = doc.get("output_channel").and_then(|v| v.as_array_of_tables()) else {
        return result;
    };

    for channel in channels.iter() {
        if let Some(sources) = channel.get("sources").and_then(|v| v.as_array()) {
            for src in sources.iter() {
                if let Some(name) = src.as_str() {
                    result.insert(name.to_string());
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CONFIG: &str = r#"
[pail]
version = 1

[telegram]
enabled = true

# My sources
[[source]]
name = "Tech Ukraine"
type = "telegram_channel"
tg_username = "tech_ukraine"
description = "Ukrainian tech news"

[[source]]
name = "Hacker News"
type = "rss"
url = "https://hnrss.org/frontpage"

[[source]]
name = "My Folder"
type = "telegram_folder"
tg_folder_name = "Tech"

[[output_channel]]
name = "Tech Digest"
slug = "tech-digest"
sources = ["Tech Ukraine", "Hacker News", "My Folder"]
schedule = "at:08:00"
prompt = "Write a digest"
"#;

    #[test]
    fn test_parse_document() {
        let doc = parse_document(SAMPLE_CONFIG).unwrap();
        assert!(doc.get("pail").is_some());
    }

    #[test]
    fn test_get_all_source_names() {
        let doc = parse_document(SAMPLE_CONFIG).unwrap();
        let names = get_all_source_names(&doc);
        assert_eq!(names, vec!["Tech Ukraine", "Hacker News", "My Folder"]);
    }

    #[test]
    fn test_add_source() {
        let mut doc = parse_document(SAMPLE_CONFIG).unwrap();
        let source = NewSource {
            name: "New Channel".to_string(),
            source_type: "telegram_channel".to_string(),
            tg_username: Some("new_channel".to_string()),
            tg_id: Some(12345),
            tg_folder_name: None,
            description: Some("A new channel".to_string()),
        };

        add_source(&mut doc, &source);

        let names = get_all_source_names(&doc);
        assert!(names.contains(&"New Channel".to_string()));

        let rendered = render(&doc);
        assert!(rendered.contains("new_channel"));
        assert!(rendered.contains("12345"));
        // Comments should be preserved
        assert!(rendered.contains("# My sources"));
    }

    #[test]
    fn test_remove_source() {
        let mut doc = parse_document(SAMPLE_CONFIG).unwrap();

        // Remove a non-first source to avoid comment attachment issues
        let result = remove_source(&mut doc, "My Folder");
        assert!(result);

        let names = get_all_source_names(&doc);
        assert!(!names.contains(&"My Folder".to_string()));
        assert_eq!(names.len(), 2);

        // The first source and its preceding comment should still be there
        let rendered = render(&doc);
        assert!(rendered.contains("# My sources"));
        assert!(rendered.contains("Tech Ukraine"));
    }

    #[test]
    fn test_remove_source_not_found() {
        let mut doc = parse_document(SAMPLE_CONFIG).unwrap();
        let result = remove_source(&mut doc, "Nonexistent");
        assert!(!result);
    }

    #[test]
    fn test_preserves_comments() {
        let mut doc = parse_document(SAMPLE_CONFIG).unwrap();

        // Do a round-trip and verify comments are preserved
        let source = NewSource {
            name: "Added".to_string(),
            source_type: "telegram_channel".to_string(),
            tg_username: Some("added".to_string()),
            tg_id: None,
            tg_folder_name: None,

            description: None,
        };
        add_source(&mut doc, &source);

        let rendered = render(&doc);
        assert!(rendered.contains("# My sources"));
    }

    #[test]
    fn test_add_source_to_empty_doc() {
        let mut doc = parse_document("[pail]\nversion = 1\n").unwrap();
        let source = NewSource {
            name: "First".to_string(),
            source_type: "telegram_channel".to_string(),
            tg_username: Some("first".to_string()),
            tg_id: None,
            tg_folder_name: None,

            description: None,
        };

        add_source(&mut doc, &source);

        let names = get_all_source_names(&doc);
        assert_eq!(names, vec!["First"]);
    }

    #[test]
    fn test_get_output_channel_names() {
        let doc = parse_document(SAMPLE_CONFIG).unwrap();
        let names = get_output_channel_names(&doc);
        assert_eq!(names, vec!["Tech Digest"]);
    }

    #[test]
    fn test_get_output_channel_names_empty() {
        let doc = parse_document("[pail]\nversion = 1\n").unwrap();
        let names = get_output_channel_names(&doc);
        assert!(names.is_empty());
    }

    #[test]
    fn test_get_channel_sources() {
        let doc = parse_document(SAMPLE_CONFIG).unwrap();
        let sources = get_channel_sources(&doc, "Tech Digest");
        assert_eq!(sources, vec!["Tech Ukraine", "Hacker News", "My Folder"]);
    }

    #[test]
    fn test_get_channel_sources_nonexistent() {
        let doc = parse_document(SAMPLE_CONFIG).unwrap();
        let sources = get_channel_sources(&doc, "Nonexistent");
        assert!(sources.is_empty());
    }

    #[test]
    fn test_set_channel_sources() {
        let mut doc = parse_document(SAMPLE_CONFIG).unwrap();
        let new_sources = vec!["Tech Ukraine".to_string(), "New Source".to_string()];
        let result = set_channel_sources(&mut doc, "Tech Digest", &new_sources);
        assert!(result);

        let sources = get_channel_sources(&doc, "Tech Digest");
        assert_eq!(sources, vec!["Tech Ukraine", "New Source"]);
    }

    #[test]
    fn test_set_channel_sources_nonexistent() {
        let mut doc = parse_document(SAMPLE_CONFIG).unwrap();
        let result = set_channel_sources(&mut doc, "Nonexistent", &["foo".to_string()]);
        assert!(!result);
    }

    #[test]
    fn test_get_tg_sources_detailed() {
        let doc = parse_document(SAMPLE_CONFIG).unwrap();
        let sources = get_tg_sources_detailed(&doc);
        assert_eq!(sources.len(), 2);

        assert_eq!(sources[0].name, "Tech Ukraine");
        assert_eq!(sources[0].tg_username.as_deref(), Some("tech_ukraine"));
        assert!(sources[0].tg_id.is_none());
        assert!(sources[0].tg_folder_name.is_none());

        assert_eq!(sources[1].name, "My Folder");
        assert_eq!(sources[1].tg_folder_name.as_deref(), Some("Tech"));
    }

    #[test]
    fn test_get_tg_sources_detailed_skips_rss() {
        let doc = parse_document(SAMPLE_CONFIG).unwrap();
        let sources = get_tg_sources_detailed(&doc);
        assert!(sources.iter().all(|s| s.name != "Hacker News"));
    }

    #[test]
    fn test_get_all_source_names_in_any_channel() {
        let doc = parse_document(SAMPLE_CONFIG).unwrap();
        let names = get_all_source_names_in_any_channel(&doc);
        assert_eq!(names.len(), 3);
        assert!(names.contains("Tech Ukraine"));
        assert!(names.contains("Hacker News"));
        assert!(names.contains("My Folder"));
    }

    #[test]
    fn test_get_all_source_names_in_any_channel_multi() {
        let config = r#"
[pail]
version = 1

[[source]]
name = "A"
type = "telegram_channel"

[[source]]
name = "B"
type = "telegram_channel"

[[source]]
name = "Orphan"
type = "telegram_channel"

[[output_channel]]
name = "Ch1"
slug = "ch1"
sources = ["A"]
prompt = "test"

[[output_channel]]
name = "Ch2"
slug = "ch2"
sources = ["A", "B"]
prompt = "test"
"#;
        let doc = parse_document(config).unwrap();
        let names = get_all_source_names_in_any_channel(&doc);
        assert_eq!(names.len(), 2);
        assert!(names.contains("A"));
        assert!(names.contains("B"));
        assert!(!names.contains("Orphan"));
    }
}
