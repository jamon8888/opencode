use gdpr_mcp::proxy::{collect_text_slots, write_text_slots, MessageContent, ChatMessage, ContentPart};
use serde_json::json;

fn text_msg(role: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role:    role.to_string(),
        content: MessageContent::Text(content.to_string()),
        extra:   json!({}),
    }
}

fn parts_msg(role: &str, texts: &[&str]) -> ChatMessage {
    let parts = texts.iter().map(|t| ContentPart {
        part_type: "text".to_string(),
        text:      Some(t.to_string()),
        extra:     json!({}),
    }).collect();
    ChatMessage { role: role.to_string(), content: MessageContent::Parts(parts), extra: json!({}) }
}

#[test]
fn test_collect_slots_from_text_messages() {
    let msgs = vec![
        text_msg("user",      "hello alice@example.com"),
        text_msg("assistant", "acknowledged"),
    ];
    let (texts, slots) = collect_text_slots(&msgs);
    assert_eq!(texts.len(), 2);
    assert_eq!(slots.len(), 2);
    assert_eq!(texts[0], "hello alice@example.com");
    assert_eq!(slots[0], (0usize, None));
    assert_eq!(slots[1], (1usize, None));
}

#[test]
fn test_collect_slots_from_parts_messages() {
    let msgs = vec![parts_msg("user", &["text A", "text B"])];
    let (texts, slots) = collect_text_slots(&msgs);
    assert_eq!(texts.len(), 2);
    assert_eq!(slots[0], (0usize, Some(0)));
    assert_eq!(slots[1], (0usize, Some(1)));
}

#[test]
fn test_write_slots_updates_text_messages() {
    let mut msgs = vec![text_msg("user", "original")];
    let slots = vec![(0usize, None)];
    write_text_slots(&mut msgs, &["cleaned".to_string()], &slots);
    match &msgs[0].content {
        MessageContent::Text(t) => assert_eq!(t, "cleaned"),
        _ => panic!("expected Text"),
    }
}

#[test]
fn test_write_slots_updates_parts_messages() {
    let mut msgs = vec![parts_msg("user", &["part0", "part1"])];
    let slots = vec![(0usize, Some(0)), (0usize, Some(1))];
    write_text_slots(&mut msgs, &["clean0".to_string(), "clean1".to_string()], &slots);
    match &msgs[0].content {
        MessageContent::Parts(parts) => {
            assert_eq!(parts[0].text.as_deref(), Some("clean0"));
            assert_eq!(parts[1].text.as_deref(), Some("clean1"));
        }
        _ => panic!("expected Parts"),
    }
}

#[test]
fn test_collect_then_write_is_roundtrip() {
    let original = "hello@example.com is a PII";
    let mut msgs  = vec![text_msg("user", original)];
    let (texts, slots) = collect_text_slots(&msgs);
    let cleaned: Vec<String> = texts.iter().map(|t| t.replace("hello@example.com", "EMAIL_1")).collect::<Vec<_>>();
    write_text_slots(&mut msgs, &cleaned, &slots);
    match &msgs[0].content {
        MessageContent::Text(t) => assert!(t.contains("EMAIL_1") && !t.contains("hello@example.com")),
        _ => panic!(),
    }
}
