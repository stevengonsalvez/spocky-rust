//! The initdb runner's command parser (`Ye` over the bundled `shell-quote`
//! `parse`): the leading words of a command, up to the first operator,
//! comment or glob.

/// Words of `command` before the first shell operator.
#[must_use]
pub fn command_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut glob = false;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        if let Some(open) = quote {
            if character == open {
                quote = None;
            } else if open == '"' && character == '\\' {
                match characters.peek().copied() {
                    Some(next @ ('"' | '\\' | '$')) => {
                        current.push(next);
                        characters.next();
                    }
                    Some(next) => {
                        current.push('\\');
                        current.push(next);
                        characters.next();
                    }
                    None => current.push('\\'),
                }
            } else {
                current.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => {
                quote = Some(character);
                in_word = true;
            }
            '\\' => {
                if let Some(next) = characters.next() {
                    current.push(next);
                }
                in_word = true;
            }
            ' ' | '\t' | '\n' => {
                if in_word {
                    if glob {
                        return words;
                    }
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '|' | '&' | ';' | '(' | ')' | '<' | '>' => {
                if in_word && !glob {
                    words.push(std::mem::take(&mut current));
                }
                return words;
            }
            '#' if !in_word => return words,
            '*' | '?' => {
                glob = true;
                current.push(character);
                in_word = true;
            }
            other => {
                current.push(other);
                in_word = true;
            }
        }
    }
    if in_word && !glob {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initdb_commands_stop_at_redirects() {
        assert_eq!(
            command_words(
                "\"/pglite/bin/postgres\" --check -F -c log_checkpoints=false < \"/dev/null\" > \"/dev/null\" 2>&1"
            ),
            [
                "/pglite/bin/postgres",
                "--check",
                "-F",
                "-c",
                "log_checkpoints=false"
            ]
        );
        assert_eq!(
            command_words("\"/pglite/bin/postgres\" -V"),
            ["/pglite/bin/postgres", "-V"]
        );
        assert_eq!(command_words("a 'b c' d2>x"), ["a", "b c", "d2"]);
    }
}
