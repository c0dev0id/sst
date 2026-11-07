//! The mention codec. On the wire a mention is a U+FFFC placeholder in the
//! body plus a BodyRange whose offsets are UTF-16 code units and whose value
//! is the mentioned ACI; `@Name` exists only in the input buffer and on screen.

use std::collections::HashMap;

use presage::libsignal_service::content::Content;
use presage::libsignal_service::prelude::Uuid;
use presage::libsignal_service::proto::{BodyRange, body_range::AssociatedValue};

use crate::signal::text_message_of;

/// Mention ranges as (start, length, aci); offsets are UTF-16 code units as on the wire.
pub(crate) fn mention_ranges(ranges: &[BodyRange]) -> impl Iterator<Item = (usize, usize, Uuid)> + '_ {
    ranges.iter().filter_map(|r| {
        let aci = match r.associated_value.as_ref()? {
            AssociatedValue::MentionAciBinary(bytes) => Uuid::from_slice(bytes).ok()?,
            AssociatedValue::MentionAci(s) => Uuid::parse_str(s).ok()?,
            AssociatedValue::Style(_) => return None,
        };
        Some((r.start? as usize, r.length? as usize, aci))
    })
}

/// Replace each mention placeholder (U+FFFC) in `body` with `@Name`.
pub(crate) fn substitute_mentions(body: &str, ranges: &[BodyRange], name_of: impl Fn(Uuid) -> String) -> String {
    let mut mentions: Vec<(usize, usize, Uuid)> = mention_ranges(ranges).collect();
    if mentions.is_empty() {
        return body.to_string();
    }
    let mut units: Vec<u16> = body.encode_utf16().collect();
    // Splice from the end so earlier offsets stay valid.
    mentions.sort_by_key(|&(start, _, _)| std::cmp::Reverse(start));
    for (start, len, aci) in mentions {
        let end = start + len;
        if len == 0 || end > units.len() {
            continue;
        }
        let label: Vec<u16> = format!("@{}", name_of(aci)).encode_utf16().collect();
        units.splice(start..end, label);
    }
    String::from_utf16_lossy(&units)
}

/// Whether the message @-mentions `aci`.
pub fn mentions_account(content: &Content, aci: Uuid) -> bool {
    mentioned_acis(content).contains(&aci)
}

/// Accounts mentioned in the message body.
pub fn mentioned_acis(content: &Content) -> Vec<Uuid> {
    text_message_of(content)
        .map(|msg| mention_ranges(&msg.body_ranges).map(|(_, _, aci)| aci).collect())
        .unwrap_or_default()
}

/// Turn `@Name` tokens into Signal mentions. A token matching a known name becomes
/// a U+FFFC placeholder plus a BodyRange carrying that account's ACI.
pub fn encode_mentions(text: &str, names: &HashMap<Uuid, String>) -> (String, Vec<BodyRange>) {
    // Longest name first so "Stefan Hagen" wins over "Stefan".
    let mut known: Vec<(&str, Uuid)> = names
        .iter()
        .filter(|(_, name)| !name.is_empty())
        .map(|(aci, name)| (name.as_str(), *aci))
        .collect();
    known.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));

    let mut body = String::with_capacity(text.len());
    let mut utf16_len = 0usize;
    let mut ranges = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let (plain, tail) = rest.split_at(at);
        body.push_str(plain);
        utf16_len += plain.encode_utf16().count();
        let candidate = &tail[1..];
        let matched = if at_word_boundary(&body) {
            known
                .iter()
                .find(|(name, _)| candidate.starts_with(*name) && word_ends(&candidate[name.len()..]))
        } else {
            None
        };
        match matched {
            Some((name, aci)) => {
                ranges.push(BodyRange {
                    start: Some(utf16_len as u32),
                    length: Some(1),
                    associated_value: Some(AssociatedValue::MentionAci(aci.to_string())),
                });
                body.push('\u{FFFC}');
                utf16_len += 1;
                rest = &candidate[name.len()..];
            }
            None => {
                body.push('@');
                utf16_len += 1;
                rest = candidate;
            }
        }
    }
    body.push_str(rest);
    (body, ranges)
}

/// True unless `before` ends inside a word, which would make the `@` part of an address.
fn at_word_boundary(before: &str) -> bool {
    before.chars().next_back().is_none_or(|c| !c.is_alphanumeric())
}

fn word_ends(after: &str) -> bool {
    after.chars().next().is_none_or(|c| !c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::name_or_uuid;

    fn alice() -> Uuid {
        Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
    }

    fn bob() -> Uuid {
        Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
    }

    fn mention(start: u32, aci: Uuid) -> BodyRange {
        BodyRange {
            start: Some(start),
            length: Some(1),
            associated_value: Some(AssociatedValue::MentionAci(aci.to_string())),
        }
    }

    fn names(uuid: Uuid) -> String {
        if uuid == alice() {
            "Alice".to_string()
        } else if uuid == bob() {
            "Bob".to_string()
        } else {
            uuid.to_string()
        }
    }

    fn roster() -> HashMap<Uuid, String> {
        HashMap::from([(alice(), "Alice".to_string()), (bob(), "Bob Builder".to_string())])
    }

    fn aci_of(range: &BodyRange) -> Uuid {
        match range.associated_value.as_ref().unwrap() {
            AssociatedValue::MentionAci(s) => Uuid::parse_str(s).unwrap(),
            other => panic!("unexpected value {other:?}"),
        }
    }

    #[test]
    fn substitutes_placeholder_with_name() {
        let out = substitute_mentions("\u{FFFC} hi", &[mention(0, alice())], names);
        assert_eq!(out, "@Alice hi");
    }

    #[test]
    fn offsets_are_utf16_units() {
        // 😀 is two UTF-16 code units, so the placeholder sits at index 3.
        let out = substitute_mentions("😀 \u{FFFC}!", &[mention(3, alice())], names);
        assert_eq!(out, "😀 @Alice!");
    }

    #[test]
    fn multiple_mentions_in_any_order() {
        let body = "\u{FFFC} and \u{FFFC}";
        let ranges = [mention(0, alice()), mention(6, bob())];
        assert_eq!(substitute_mentions(body, &ranges, names), "@Alice and @Bob");
        let reversed = [mention(6, bob()), mention(0, alice())];
        assert_eq!(substitute_mentions(body, &reversed, names), "@Alice and @Bob");
    }

    #[test]
    fn out_of_range_mention_is_ignored() {
        let out = substitute_mentions("x", &[mention(5, alice())], names);
        assert_eq!(out, "x");
    }

    #[test]
    fn binary_aci_is_accepted() {
        let range = BodyRange {
            start: Some(0),
            length: Some(1),
            associated_value: Some(AssociatedValue::MentionAciBinary(bob().as_bytes().to_vec())),
        };
        assert_eq!(substitute_mentions("\u{FFFC}", &[range], names), "@Bob");
    }

    #[test]
    fn style_ranges_are_not_mentions() {
        let range = BodyRange {
            start: Some(0),
            length: Some(4),
            associated_value: Some(AssociatedValue::Style(1)),
        };
        assert_eq!(substitute_mentions("bold", &[range], names), "bold");
    }

    #[test]
    fn encodes_known_name_as_placeholder_and_range() {
        let (body, ranges) = encode_mentions("hi @Alice, ok?", &roster());
        assert_eq!(body, "hi \u{FFFC}, ok?");
        assert_eq!(ranges.len(), 1);
        assert_eq!((ranges[0].start, ranges[0].length), (Some(3), Some(1)));
        assert_eq!(aci_of(&ranges[0]), alice());
    }

    #[test]
    fn encoded_offsets_count_utf16_units() {
        let (body, ranges) = encode_mentions("😀 @Alice", &roster());
        assert_eq!(body, "😀 \u{FFFC}");
        assert_eq!(ranges[0].start, Some(3));
    }

    #[test]
    fn names_with_spaces_match_whole() {
        let (body, ranges) = encode_mentions("@Bob Builder and @Alice", &roster());
        assert_eq!(body, "\u{FFFC} and \u{FFFC}");
        assert_eq!(ranges.iter().map(aci_of).collect::<Vec<_>>(), vec![bob(), alice()]);
        assert_eq!(ranges[1].start, Some(6));
    }

    #[test]
    fn longer_name_wins_over_its_prefix() {
        let names = HashMap::from([(alice(), "Stefan".to_string()), (bob(), "Stefan Hagen".to_string())]);
        let (_, ranges) = encode_mentions("@Stefan Hagen hi", &names);
        assert_eq!(aci_of(&ranges[0]), bob());
        let (_, ranges) = encode_mentions("@Stefan hi", &names);
        assert_eq!(aci_of(&ranges[0]), alice());
    }

    #[test]
    fn partial_words_and_addresses_stay_text() {
        for text in ["@Alicia", "@Alicex", "mail me@Alice.org", "@Nobody"] {
            let (body, ranges) = encode_mentions(text, &roster());
            assert_eq!(body, text);
            assert!(ranges.is_empty());
        }
    }

    #[test]
    fn mentions_round_trip() {
        let text = "ping @Alice and @Bob Builder 😀 @Alice";
        let (body, ranges) = encode_mentions(text, &roster());
        let shown = substitute_mentions(&body, &ranges, |u| name_or_uuid(&roster(), u));
        assert_eq!(shown, text);
    }
}
