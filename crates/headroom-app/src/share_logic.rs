//! What a share image shows and the `share-image` action's input. Pure, so it tests without GTK.

use headroom_core::history::BreakdownView;
use headroom_core::view::Group;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// A group or section title.
    Heading(String),
    /// A name on the left and a value on the right (either may be empty), after the icon of the provider
    /// `icon` names (`ProviderKind::id()`; empty for none).
    Line { name: String, value: String, icon: String },
    /// A bar filled from 0 to 1.
    Meter(f64),
    /// Wrapped secondary text.
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShareCard {
    pub title: Option<String>,
    /// The provider (`ProviderKind::id()`) whose icon goes before the title; empty for none.
    pub icon: String,
    pub blocks: Vec<Block>,
}

/// Drops every email-looking word, and the " · " separators it leaves empty ("OpenCode · a@b.com" is "OpenCode").
pub fn strip_emails(text: &str) -> String {
    let is_email = |word: &str| {
        let word = word.trim_matches(|c: char| "()<>,;:\"'[]".contains(c));
        word.split_once('@').is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.') && !domain.ends_with('.') && !domain.contains('@'))
    };
    text.split(" · ")
        .map(|part| part.split_whitespace().filter(|w| !is_email(w)).collect::<Vec<_>>().join(" "))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The Priority tab: each group's title, then its rows (title and value, a meter, the subtitle).
pub fn priority_card(groups: &[Group]) -> ShareCard {
    let mut blocks = vec![];
    for group in groups {
        if !group.title.is_empty() {
            blocks.push(Block::Heading(strip_emails(&group.title)));
        }
        for row in &group.rows {
            blocks.push(Block::Line { name: strip_emails(&row.title), value: strip_emails(row.value.as_deref().unwrap_or_default()), icon: row.kind.clone() });
            if let Some(fraction) = row.meter {
                blocks.push(Block::Meter(fraction));
            }
            let subtitle = strip_emails(&row.subtitle);
            if !subtitle.is_empty() {
                blocks.push(Block::Text(subtitle));
            }
        }
    }
    ShareCard { title: Some("Priority".into()), icon: String::new(), blocks }
}

/// One history row's breakdown under the provider's name, as the breakdown panel lists it.
pub fn breakdown_card(provider: &str, kind: &str, breakdown: &BreakdownView) -> ShareCard {
    let mut blocks = vec![Block::Heading(strip_emails(&breakdown.title))];
    if breakdown.empty {
        blocks.push(Block::Text(format!("{} didn't report any data for this range.", strip_emails(provider))));
    } else {
        if !breakdown.figures.is_empty() {
            blocks.push(Block::Text(breakdown.figures.join(" · ")));
        }
        for part in &breakdown.token_parts {
            blocks.push(Block::Line { name: part.title.clone(), value: part.value.clone(), icon: String::new() });
            if let Some(fraction) = part.fraction {
                blocks.push(Block::Meter(fraction));
            }
        }
        if !breakdown.models.is_empty() {
            blocks.push(Block::Line { name: "Top Models".into(), value: breakdown.models_label.clone().unwrap_or_default(), icon: String::new() });
            blocks.extend(breakdown.models.iter().map(|m| Block::Line { name: strip_emails(&m.name), value: m.value.clone(), icon: String::new() }));
        }
    }
    ShareCard { title: Some(strip_emails(provider)), icon: kind.into(), blocks }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Priority,
    Breakdown { row_id: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShareRequest {
    pub kind: Kind,
    pub request_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Raw {
    kind: String,
    row_id: Option<String>,
    request_id: String,
}

/// The `share-image` action's parameter: `{"kind":"priority"|"breakdown","rowId":..,"requestId":..}`. The request
/// id becomes part of a file name, so it has to be a plain token.
pub fn parse_share_request(text: &str) -> Result<ShareRequest, String> {
    let raw: Raw = serde_json::from_str(text).map_err(|e| format!("not a share request: {e}"))?;
    if !(1..=64).contains(&raw.request_id.len()) || !raw.request_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err("a share request needs a request id of letters, digits and '-' (up to 64)".into());
    }
    let kind = match (raw.kind.as_str(), raw.row_id) {
        ("priority", _) => Kind::Priority,
        ("breakdown", Some(row_id)) if !row_id.is_empty() => Kind::Breakdown { row_id },
        ("breakdown", _) => return Err("a breakdown share request needs a row id".into()),
        (other, _) => return Err(format!("unknown share kind {other:?}")),
    };
    Ok(ShareRequest { kind, request_id: raw.request_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use headroom_core::history::{ModelLine, TokenPartLine};
    use headroom_core::view::Row;

    #[test]
    fn emails_are_left_out_with_their_separators() {
        assert_eq!(strip_emails("OpenCode · a@b.com"), "OpenCode");
        assert_eq!(strip_emails("a@b.com · Pro · resets in 3h"), "Pro · resets in 3h");
        assert_eq!(strip_emails("Work (me.x+y@mail.example.com) plan"), "Work plan");
        assert_eq!(strip_emails("Session · 40% used"), "Session · 40% used");
        assert_eq!(strip_emails("a@b.com"), "");
        // Not emails: left alone.
        assert_eq!(strip_emails("@home and x@ and 5 @ 3"), "@home and x@ and 5 @ 3");
    }

    #[test]
    fn the_priority_card_lists_groups_rows_and_meters_without_emails() {
        let group = Group {
            title: "Running Out".into(),
            kind: String::new(),
            unifiable: false,
            unified: false,
            rows: vec![
                Row { title: "Session".into(), subtitle: "Claude · a@b.com · resets in 3h".into(), meter: Some(0.4), value: Some("40% used".into()), kind: "claude".into(), ..Row::default() },
                Row { title: "All Clear".into(), subtitle: "a@b.com".into(), ..Row::default() },
            ],
        };
        let card = priority_card(&[group]);
        assert_eq!(
            card.blocks,
            vec![
                Block::Heading("Running Out".into()),
                Block::Line { name: "Session".into(), value: "40% used".into(), icon: "claude".into() },
                Block::Meter(0.4),
                Block::Text("Claude · resets in 3h".into()),
                Block::Line { name: "All Clear".into(), value: String::new(), icon: String::new() },
            ]
        );
    }

    #[test]
    fn the_breakdown_card_mirrors_the_panel() {
        let view = BreakdownView {
            title: "Today".into(),
            empty: false,
            figures: vec!["$1.50".into(), "3K tokens".into()],
            token_parts: vec![TokenPartLine { title: "Input".into(), value: "2K".into(), fraction: Some(0.5) }, TokenPartLine { title: "Output".into(), value: "1K".into(), fraction: None }],
            models_label: Some("Cost".into()),
            models: vec![ModelLine { name: "m1".into(), value: "$1".into() }],
        };
        let card = breakdown_card("OpenCode · a@b.com", "opencode", &view);
        assert_eq!(card.title.as_deref(), Some("OpenCode"));
        assert_eq!(card.icon, "opencode");
        assert_eq!(card.blocks.len(), 7);
        assert_eq!(card.blocks[3], Block::Meter(0.5));
        assert_eq!(card.blocks[5], Block::Line { name: "Top Models".into(), value: "Cost".into(), icon: String::new() });
        let empty = BreakdownView { empty: true, ..view };
        assert_eq!(breakdown_card("Claude", "claude", &empty).blocks[1], Block::Text("Claude didn't report any data for this range.".into()));
    }

    #[test]
    fn share_requests_parse_and_bad_input_is_refused() {
        let ok = parse_share_request(r#"{"kind":"priority","requestId":"3f2a-9"}"#).unwrap();
        assert_eq!(ok, ShareRequest { kind: Kind::Priority, request_id: "3f2a-9".into() });
        let ok = parse_share_request(r#"{"kind":"breakdown","rowId":"claude@a/Today","requestId":"r1"}"#).unwrap();
        assert_eq!(ok.kind, Kind::Breakdown { row_id: "claude@a/Today".into() });
        let long = format!(r#"{{"kind":"priority","requestId":"{}"}}"#, "a".repeat(65));
        for bad in [
            "",
            "not json",
            r#"{"kind":"breakdown","requestId":"r1"}"#,
            r#"{"kind":"breakdown","rowId":"","requestId":"r1"}"#,
            r#"{"kind":"other","requestId":"r1"}"#,
            r#"{"kind":"priority"}"#,
            r#"{"kind":"priority","requestId":""}"#,
            r#"{"kind":"priority","requestId":"../x"}"#,
            r#"{"kind":"priority","requestId":"a/b"}"#,
            r#"{"kind":"priority","requestId":"a_b"}"#,
            r#"{"kind":"priority","requestId":"a.png"}"#,
            &long,
        ] {
            assert!(parse_share_request(bad).is_err(), "{bad}");
        }
    }
}
