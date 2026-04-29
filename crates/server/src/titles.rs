use devo_protocol::{
    ModelRequest, RequestContent, RequestMessage, ResponseContent, SamplingControls,
};
/// Derives a cheap deterministic provisional session title from the first user prompt.
pub(crate) fn derive_provisional_title(input: &str) -> Option<String> {
    let mut text = strip_code_fences(input);
    text = collapse_whitespace(&text);
    text = strip_prompt_noise(&text);
    text = trim_title_candidate(&text);

    if text.len() < 8 {
        return None;
    }
    if looks_like_code_only(&text) {
        return None;
    }

    let candidate = first_clause(&text);
    let candidate = candidate.trim_matches(|ch: char| ch.is_ascii_punctuation() && ch != '\'');
    let candidate = collapse_whitespace(candidate);
    if candidate.is_empty() {
        return None;
    }

    let candidate = sentence_case(&candidate);
    let visible = candidate.chars().count();
    if !(8..=80).contains(&visible) {
        return None;
    }
    Some(candidate)
}

/// Builds a non-tool model request used to generate one final session title.
pub(crate) fn build_title_generation_request(model: String, user_input: &str) -> ModelRequest {
    ModelRequest {
        model,
        system: Some(
            "Generate a short session title. Respond with only the title in sentence case. Use 3 to 8 words. No markdown, no quotes, no trailing punctuation unless required by a proper noun.".to_string(),
        ),
        messages: vec![RequestMessage {
            role: "user".to_string(),
            content: vec![RequestContent::Text {
                text: format!(
                    "First user message:\n{user_input}\n\nReturn only the best concise title."
                ),
            }],
        }],
        max_tokens: 32,
        tools: None,
        sampling: SamplingControls {
            temperature: Some(0.0),
            ..SamplingControls::default()
        },
        thinking: None,
        reasoning_effort: None,
        extra_body: None,
    }
}

/// Extracts and normalizes one title candidate from a complete provider response.
pub(crate) fn normalize_generated_title(content: &[ResponseContent]) -> Option<String> {
    let raw = content.iter().find_map(|block| match block {
        ResponseContent::Text(text) => Some(text.as_str()),
        ResponseContent::ToolUse { .. } => None,
    })?;
    let line = raw.lines().next()?.trim();
    let line = line.trim_matches(|ch| matches!(ch, '"' | '\'' | '#' | '`' | ' '));
    if line.is_empty() {
        return None;
    }
    let collapsed = collapse_whitespace(line);
    let without_trailing = collapsed
        .trim_end_matches(['.', '!', '?', ':', ';'])
        .to_string();
    let candidate = sentence_case(without_trailing.trim());
    let visible = candidate.chars().count();
    if !(3..=80).contains(&visible) {
        return None;
    }
    Some(candidate)
}

fn strip_code_fences(input: &str) -> String {
    let mut output = String::new();
    let mut inside_fence = false;
    for line in input.lines() {
        if line.trim_start().starts_with("```") {
            inside_fence = !inside_fence;
            continue;
        }
        if !inside_fence {
            output.push_str(line);
            output.push('\n');
        }
    }
    output
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_prompt_noise(input: &str) -> String {
    input
        .trim()
        .trim_start_matches('>')
        .trim_start_matches('$')
        .trim_start_matches('#')
        .trim()
        .to_string()
}

fn trim_title_candidate(input: &str) -> String {
    let compact = input.trim();
    compact.chars().take(160).collect::<String>()
}

fn looks_like_code_only(input: &str) -> bool {
    let alpha_count = input.chars().filter(|ch| ch.is_alphabetic()).count();
    let symbol_count = input
        .chars()
        .filter(|ch| !ch.is_alphanumeric() && !ch.is_whitespace())
        .count();
    alpha_count < 4 || symbol_count > alpha_count * 2
}

fn first_clause(input: &str) -> &str {
    input
        .split(['.', '!', '?', '\n', ';', ':'])
        .next()
        .unwrap_or(input)
}

fn sentence_case(input: &str) -> String {
    let mut chars = input.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    format!("{}{}", first.to_uppercase(), chars.as_str())
}

#[cfg(test)]
mod tests {
    use devo_protocol::ResponseContent;
    use pretty_assertions::assert_eq;

    use super::{derive_provisional_title, normalize_generated_title};

    #[test]
    fn derives_title_from_plain_text_prompt() {
        assert_eq!(
            derive_provisional_title("help me add rollout persistence to the server"),
            Some("Help me add rollout persistence to the server".to_string())
        );
    }

    #[test]
    fn ignores_fenced_code_only_input() {
        assert_eq!(derive_provisional_title("```rust\nfn main() {}\n```"), None);
    }

    #[test]
    fn trims_shell_prompt_noise() {
        assert_eq!(
            derive_provisional_title("> list the current sessions and switch to the newest one"),
            Some("List the current sessions and switch to the newest one".to_string())
        );
    }

    #[test]
    fn normalizes_generated_title_text() {
        assert_eq!(
            normalize_generated_title(&[ResponseContent::Text(
                "\"rollout persistence follow up.\"\nextra".to_string()
            )]),
            Some("Rollout persistence follow up".to_string())
        );
    }
}
