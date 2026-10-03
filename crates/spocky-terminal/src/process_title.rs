//! Terminal titles and command lifecycle text, following pinned
//! `packages/server/src/terminal/terminal.ts`: `normalizeProcessTitle`,
//! `humanizeProcessTitle`, the initial title of a profile command, and the
//! OSC 633 command-finished payload.

/// JavaScript `WhiteSpace` and `LineTerminator`, which `trim` and `\s` use.
#[must_use]
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `String.prototype.trim`.
#[must_use]
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// Node `path.posix.basename(path)`.
#[must_use]
pub fn basename(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

/// `^[A-Za-z_][A-Za-z0-9_]*=` and the index after `=`.
fn assignment_prefix_len(token: &str) -> Option<usize> {
    let bytes = token.as_bytes();
    if !bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
    {
        return None;
    }
    let name_len = bytes
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))?;
    (bytes[name_len] == b'=').then_some(name_len + 1)
}

fn normalize_process_token(token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }
    let quote = if token.starts_with('"') && token.ends_with('"') {
        Some('"')
    } else if token.starts_with('\'') && token.ends_with('\'') {
        Some('\'')
    } else {
        None
    };
    // A one-character quote token slices to empty, like `slice(1, -1)`.
    let raw = match quote {
        Some(_) if token.len() < 2 => "",
        Some(_) => &token[1..token.len() - 1],
        None => token,
    };
    if raw.is_empty() {
        return token.to_owned();
    }
    // `^(NAME=)(.+)$`: the value must be non-empty.
    let (prefix, value) = match assignment_prefix_len(raw) {
        Some(end) if end < raw.len() => raw.split_at(end),
        _ => ("", raw),
    };
    if !value.contains('/') {
        return token.to_owned();
    }
    let normalized = format!("{prefix}{}", basename(value));
    match quote {
        Some(quote) => format!("{quote}{normalized}{quote}"),
        None => normalized,
    }
}

/// `normalizeProcessTitle`.
#[must_use]
pub fn normalize_process_title(process_title: &str) -> Option<String> {
    let trimmed = js_trim(process_title);
    if trimmed.is_empty() {
        return None;
    }
    let mut collapsed = String::new();
    let mut in_space = false;
    for c in trimmed.chars() {
        if is_js_whitespace(c) {
            if !in_space {
                collapsed.push(' ');
            }
            in_space = true;
        } else {
            collapsed.push(c);
            in_space = false;
        }
    }
    let normalized = collapsed
        .split(' ')
        .map(normalize_process_token)
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = js_trim(&normalized);
    (!normalized.is_empty()).then(|| normalized.to_owned())
}

const PROCESS_INTERPRETERS: [&str; 11] = [
    "bash", "bun", "deno", "node", "nodejs", "python", "python3", "ruby", "sh", "tsx", "zsh",
];

fn package_manager_for_script(script: &str) -> Option<&'static str> {
    Some(match script {
        "bun.js" => "bun",
        "npm-cli.js" => "npm",
        "npx-cli.js" => "npx",
        "pnpm.cjs" | "pnpm.js" => "pnpm",
        "yarn.cjs" | "yarn.js" => "yarn",
        _ => return None,
    })
}

/// `humanizeProcessTitle`.
#[must_use]
pub fn humanize_process_title(process_title: &str) -> Option<String> {
    let normalized = normalize_process_title(process_title)?;
    let mut tokens: Vec<&str> = normalized.split(' ').filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return None;
    }
    while tokens.first() == Some(&"env") {
        tokens.remove(0);
        while tokens
            .first()
            .is_some_and(|token| assignment_prefix_len(token).is_some())
        {
            tokens.remove(0);
        }
    }
    let (Some(first), second) = (tokens.first().copied(), tokens.get(1).copied()) else {
        return Some(normalized);
    };
    if let Some(second) = second
        && PROCESS_INTERPRETERS.contains(&first)
    {
        let rest = &tokens[2..];
        if let Some(manager) = package_manager_for_script(second) {
            let joined = std::iter::once(manager)
                .chain(rest.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            let joined = js_trim(&joined);
            return Some(if joined.is_empty() { manager } else { joined }.to_owned());
        }
        if !second.starts_with('-') {
            let joined = std::iter::once(second)
                .chain(rest.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            return Some(js_trim(&joined).to_owned());
        }
    }
    Some(normalized)
}

/// The title a terminal starts with: a non-blank preset title trimmed, else
/// the humanized (or normalized) `[command, ...args].join(" ")`.
#[must_use]
pub fn initial_title(
    preset: Option<&str>,
    command: Option<&str>,
    args: &[String],
) -> Option<String> {
    if let Some(title) = preset.map(js_trim).filter(|title| !title.is_empty()) {
        return Some(title.to_owned());
    }
    let command = command.filter(|command| !command.is_empty())?;
    let process_title = std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    humanize_process_title(&process_title).or_else(|| normalize_process_title(&process_title))
}

/// `parseCommandFinishedOsc`: `Some(None)` is `{ exitCode: null }`.
#[must_use]
pub fn parse_command_finished_osc(data: &str) -> Option<Option<f64>> {
    let parts: Vec<&str> = data.split(';').collect();
    if parts[0] != "D" {
        return None;
    }
    if parts.len() == 1 {
        return Some(None);
    }
    let code = parts[1];
    let digits = code.strip_prefix('-').unwrap_or(code);
    if parts.len() != 2 || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // `Number("-0")` is -0 and `Number` of a long digit run rounds; Rust's
    // float parser agrees on both.
    code.parse::<f64>().ok().map(Some)
}
