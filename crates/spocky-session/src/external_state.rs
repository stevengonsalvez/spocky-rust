//! `commandMayHaveChangedExternalState` from pinned Paseo
//! `agent/agent-manager.ts`: shell commands whose effects local file
//! watchers do not see.

use crate::text::is_js_whitespace;

fn is_word(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// Matches `\b<words joined by \s+>\b` anywhere in `text`, where the last
/// word may be any of `last`.
fn contains_command(text: &[char], words: &[&str], last: &[&str]) -> bool {
    (0..text.len()).any(|start| {
        if start > 0 && is_word(text[start - 1]) {
            return false;
        }
        let mut index = start;
        for word in words {
            let Some(after) = match_word(text, index, word) else {
                return false;
            };
            let spaces = text[after..]
                .iter()
                .take_while(|character| is_js_whitespace(**character))
                .count();
            if spaces == 0 {
                return false;
            }
            index = after + spaces;
        }
        last.iter().any(|word| {
            match_word(text, index, word)
                .is_some_and(|after| after == text.len() || !is_word(text[after]))
        })
    })
}

fn match_word(text: &[char], index: usize, word: &str) -> Option<usize> {
    let mut position = index;
    for expected in word.chars() {
        if text.get(position) != Some(&expected) {
            return None;
        }
        position += 1;
    }
    Some(position)
}

/// `commandMayHaveChangedExternalState`: `gh pr merge|close|create|edit|
/// comment|review`, `git push`, or `git fetch`, matched in the lowercased
/// command with `\b` word boundaries and `\s+` separators.
#[must_use]
pub fn command_may_have_changed_external_state(command: &str) -> bool {
    let normalized: Vec<char> = command.to_lowercase().chars().collect();
    contains_command(
        &normalized,
        &["gh", "pr"],
        &["merge", "close", "create", "edit", "comment", "review"],
    ) || contains_command(&normalized, &["git"], &["push"])
        || contains_command(&normalized, &["git"], &["fetch"])
}
