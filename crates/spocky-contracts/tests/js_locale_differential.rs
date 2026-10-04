//! `toLocaleLowerCase` differential: node v22.20.0 against
//! `js_to_locale_lower_case` and `default_locale`.
//!
//! - Every Unicode scalar value under `tr`, `az`, `lt`, and `en`, and
//!   combining-mark contexts around `I`, `J`, `U+012E`, and `U+0307` (the
//!   classes 0, 216, 220, 230, 232, 240 and 1 each way), must lowercase the
//!   same.
//! - A corpus of language tags, valid and not, must accept or throw the same
//!   `RangeError` message and pick the same tailoring.
//! - Node run with `LANG`, `LC_ALL`, and `LC_MESSAGES` set to `tr_TR.UTF-8`,
//!   `az_AZ`, `lt_LT`, `en_US`, `C.UTF-8`, and others (and an empty
//!   environment) resolves the same default locale, and its no-argument
//!   `toLocaleLowerCase()` agrees. On Unix (macOS and Linux give node the same
//!   answers) every mixture of `LC_ALL`,
//!   `LC_MESSAGES`, `LC_CTYPE`, and `LANG` over a value set is compared too,
//!   with `@` modifiers (`@euro`, `@latin`, and variant-shaped ones).

#[path = "support/pinned_node.rs"]
mod support;

#[cfg(unix)]
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::Command;

use spocky_contracts::js_value::{JsValue, parse, stringify};
#[cfg(unix)]
use spocky_contracts::locale::default_locale_from;
use spocky_contracts::locale::js_to_locale_lower_case;

const NODE_SCRIPT: &str = r#"
const [dist, file] = process.argv.slice(1);
const { readFileSync } = await import("node:fs");
const { locales, exhaustive, contexts, tags } = JSON.parse(readFileSync(file, "utf8"));
const out = [];
const hex = (text) => [...text].map((c) => c.codePointAt(0).toString(16)).join(",");
for (const locale of locales) {
  if (exhaustive.includes(locale)) {
    for (let code = 0; code <= 0x10ffff; code++) {
      if (code >= 0xd800 && code <= 0xdfff) continue;
      const text = String.fromCodePoint(code);
      const lower = text.toLocaleLowerCase(locale);
      if (lower !== text) out.push(locale + " " + code.toString(16) + ":" + hex(lower));
    }
  }
  for (const text of contexts) out.push(locale + " " + JSON.stringify(text.toLocaleLowerCase(locale)));
}
for (const tag of tags) {
  try {
    out.push("tag " + JSON.stringify("Iİ̇".toLocaleLowerCase(tag)));
  } catch (error) {
    out.push("tag " + error.name + ": " + error.message);
  }
}
process.stdout.write(out.join("\n") + "\n");
"#;

const LOCALES: &[&str] = &["tr", "az", "lt", "en"];
/// Locales whose every scalar value is compared (`az` and `en` share the
/// mappings of `tr` and the root, which the contexts and the string
/// differential cover).
const EXHAUSTIVE: &[&str] = &["tr", "lt"];

/// Characters whose neighbours decide the tailoring, and classes 0, 1, 202,
/// 216, 220, 230, 232, and 240 to put between them.
const CONTEXT_CHARS: &[&str] = &[
    "I", "i", "J", "j", "\u{12e}", "\u{12f}", "\u{cc}", "\u{cd}", "\u{128}", "\u{130}", "\u{307}",
    "\u{300}", "\u{301}", "\u{316}", "\u{323}", "\u{345}", "\u{31b}", "\u{334}", "\u{315}",
    "\u{327}", "A", "a", "\u{3a3}", "\u{3c3}", " ", "\u{344}", "\u{1d7}",
];

/// Language tags, mixing valid, aliased, and malformed forms.
const TAGS: &[&str] = &[
    "tr",
    "tr-TR",
    "tr-Latn-TR",
    "tr-TR-u-co-x",
    "tr-u",
    "tr-u-",
    "tr-u-ca",
    "tr-u-ca-",
    "tr-u-ca-gregory-",
    "tr-t-en",
    "tr-t-en-us",
    "tr-a-bbb",
    "tr-a-bbbb-cc",
    "tr-x",
    "tr-x-",
    "tr-x-a",
    "tr-x-abcdefghi",
    "tr-X-a",
    "tr--TR",
    "-tr",
    "tr TR",
    "tr.TR",
    "tr-T",
    "tr-TRR",
    "tr-12",
    "tr-123",
    "tr-1234",
    "tr-12345",
    "tr-1abc",
    "tr-abcde",
    "tr-abcdefghi",
    "tr-Latn-Latn",
    "tr-TR-TR",
    "tr-Latn-TR-1abc",
    "tr-TR-abcde-abcde",
    "tr-TR-abcde-fghij",
    "tr-abcde-TR",
    "tr-extl",
    "tr-ext-ext",
    "tr-aaa-bbb",
    "tur-TR",
    "tur",
    "azj",
    "azb",
    "aze-AZ",
    "lit-LT",
    "tr-\u{f8}",
    "t\u{fc}r",
    "ab",
    "abc",
    "abcd",
    "abcde",
    "abcdefgh",
    "abcdefghi",
    "a-b",
    "i-tr",
    "en-u-ca-gregory",
    "en-US-POSIX",
    "en-us-posix",
    "und-tr",
    "und",
    "und-TR",
    "zxx",
    "tl",
    "fil",
    "sr-Latn",
    "tr-Cyrl",
    "tr-ZZ",
    "tr-419",
    "tr-001",
    "TR",
    "Tr",
    "tR",
    "tr-tr",
    "tr-latn-tr",
    " tr",
    "tr ",
    "tr\n",
    "\ttr",
    "tr-TR-u-ca-gregory-x-priv",
    "tr-TR-t-m0-x",
    "tr-0abc-1abc",
    "tr-1abc-0abc",
    "tr-1abc-1abc",
    "tr-abcde-1abc",
    "tr-u-ca-a-b",
    "tr-u-a-b",
    "tr-u-1-2",
    "tr-u-x",
    "tr-u-aa-bb",
    "tr-u-ab",
    "tr-u-abc",
    "tr-t-a",
    "tr-t-abc",
    "tr-t-abcde",
    "en-t-tr",
    "art-lojban",
    "zh-min-nan",
    "sgn-be-fr",
    "i-klingon",
    "no-bok",
    "x-foo",
    "en-x-tr",
    "az-Arab",
    "az-Cyrl-AZ",
    "lt-u-co-phonebk",
    "el-polyton",
    "el-u-nu-grek",
    "az-u-ca-x",
    "tr-ca",
    "tr-gregory",
    "",
    "root",
    "123",
    "a",
    "aaaaaaaaa",
    "invalid-tag",
    "tr_TR",
    "x-private",
    "en-GB-oed",
    "TUR",
    "tr-u-ca-buddhist-nu-latn",
    "tr-u-kn",
    "tr-u-kn-true",
    "tr-u-12",
    "tr-u-a1",
    "tr-u-1a",
    "tr-t-en-t0-abc",
    "tr-t-m0-abc",
    "tr-t-m0",
    "tr-t-en-us-m0-abc-d0-efg",
    "tr-t-abc-def",
    "tr-a-bb-cc-dd",
    "tr-b-cc-d-dd",
    "tr-a-bb-a-cc",
    "tr-",
    "tr-TR-",
    "tr-Latn-",
    "tr-1abc-",
    "tr-x-a-b-c",
    "tr-x-a--b",
    "tr-u-ca-gregory-ca-buddhist",
    "tr-1-2",
    "tr-9-ab",
    "tr-9",
    "lt-1abc",
    "az-Latn",
    "az-Latn-AZ-1abc-u-ca-x-y",
];

#[cfg(unix)]
/// The environments node is run under for the default locale: each is
/// `LANG`, `LC_ALL`, `LC_MESSAGES`, `LC_CTYPE` pairs.
const ENVIRONMENTS: &[&[(&str, &str)]] = &[
    &[],
    &[("LANG", "tr_TR.UTF-8")],
    &[("LANG", "az_AZ")],
    &[("LANG", "lt_LT")],
    &[("LANG", "en_US")],
    &[("LANG", "C.UTF-8")],
    &[("LANG", "C")],
    &[("LANG", "POSIX")],
    &[("LANG", "tr")],
    &[("LANG", "tr_TR")],
    &[("LANG", "tr_TR.ISO-8859-9")],
    &[("LANG", "TR_tr")],
    &[("LANG", "tr-TR")],
    &[("LANG", "zz_ZZ")],
    &[("LANG", "de_DE.UTF-8")],
    &[("LANG", "sr_Latn_RS")],
    &[("LANG", "lt_LT.UTF-8")],
    &[("LC_ALL", "tr_TR.UTF-8")],
    &[("LC_ALL", "az_AZ.UTF-8")],
    &[("LC_ALL", "en_US.UTF-8")],
    &[("LC_MESSAGES", "tr_TR")],
    &[("LC_MESSAGES", "lt_LT")],
    &[("LC_MESSAGES", "en_US")],
    &[("LC_CTYPE", "tr_TR")],
    &[("LC_ALL", "tr_TR.UTF-8"), ("LANG", "en_US")],
    &[("LC_ALL", "lt_LT.UTF-8"), ("LANG", "tr_TR")],
    &[("LC_ALL", "en_US.UTF-8"), ("LANG", "tr_TR.UTF-8")],
    &[("LANG", "az_AZ@latin")],
    &[("LANG", "de_DE@euro")],
    &[("LANG", "ca_ES@valencia")],
    &[("LANG", "sr@latin")],
    &[("LANG", "de@abcde_fghij")],
    &[("LANG", "de@fghij_abcde")],
    &[("LANG", "de@euro_abcde")],
    &[("LANG", "de@euro_ab")],
    &[("LANG", "de@abcdefghi")],
    &[("LANG", "de@12345678")],
    &[("LANG", "de@1234")],
    &[("LANG", "de@123")],
    &[("LANG", "de@calendar=islamic")],
    &[("LANG", "de@a")],
    &[("LANG", "de@a_bc")],
    &[("LANG", "de@aa_b_c")],
    &[("LANG", "de@posix")],
    &[("LANG", "en_US@posix")],
    &[("LANG", "en_GB@posix")],
    &[("LANG", "en_US_POSIX")],
    &[("LANG", "de_DE_euro")],
    &[("LANG", "de_DE_abcde")],
    &[("LANG", "de_DE.UTF-8@euro")],
    &[("LANG", "de@abcde.UTF-8")],
    &[("LANG", "de@abcde@fghij")],
    &[("LANG", "de@-abcde")],
    &[("LC_ALL", "az_AZ@latin"), ("LANG", "de_DE")],
    // Only the sorted leading variants stay variants; `-` and `_` alike
    // separate pieces; from the first other piece on, all go to x-lvariant-.
    &[("LANG", "de@ab-cde-fghij")],
    &[("LANG", "de@ab_cde_fghij")],
    &[("LANG", "de@ab-cde_fghij")],
    &[("LANG", "de@ab_cde-fghij")],
    &[("LANG", "de@ab-cde")],
    &[("LANG", "de@abcde-fghij")],
    &[("LANG", "de@euro-abcde")],
    &[("LANG", "de@abcde-euro")],
    &[("LANG", "de@abcde-fghij-euro")],
    &[("LANG", "de@euro-abcde-fghij")],
    &[("LANG", "de@euro_abcde_fghij")],
    &[("LANG", "de@ab-abcde")],
    &[("LANG", "de@abcde_ab-cde")],
    &[("LANG", "de@euro-ab-abcde")],
    &[("LANG", "de@abcde_abcde")],
];

#[cfg(unix)]
/// Node run with exactly `environment` as its environment, under `gtimeout`
/// or `timeout` so a hung spawn fails the test instead of stalling it.
fn run_node_under(
    node: &std::ffi::OsStr,
    environment: &[(&str, &str)],
    script: &str,
) -> std::process::Output {
    Command::new(support::timeout_command())
        .args(["--kill-after=5", "60", "/usr/bin/env", "-i"])
        .args(
            environment
                .iter()
                .map(|(name, value)| format!("{name}={value}")),
        )
        .arg(node)
        .args(["-e", script])
        .output()
        .expect("run pinned node")
}

/// xorshift64*, so every run builds the same strings.
struct Random(u64);

impl Random {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        usize::try_from(
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % u64::try_from(bound).expect("bound"),
        )
        .expect("index")
    }
}

fn contexts() -> Vec<String> {
    let mut out = Vec::new();
    let mut frontier: Vec<String> = vec![String::new()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for prefix in &frontier {
            for character in CONTEXT_CHARS {
                next.push(format!("{prefix}{character}"));
            }
        }
        out.extend(next.iter().cloned());
        frontier = next;
    }
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    for _ in 0..8000 {
        let length = 4 + random.below(5);
        out.push(
            (0..length)
                .map(|_| CONTEXT_CHARS[random.below(CONTEXT_CHARS.len())])
                .collect(),
        );
    }
    out
}

fn hex(text: &str) -> String {
    text.chars()
        .map(|character| format!("{:x}", u32::from(character)))
        .collect::<Vec<_>>()
        .join(",")
}

fn rust_lines(contexts: &[String]) -> String {
    let mut out = String::new();
    for locale in LOCALES {
        for code in 0..=0x0010_FFFF_u32 {
            if !EXHAUSTIVE.contains(locale) {
                break;
            }
            let Some(character) = char::from_u32(code) else {
                continue;
            };
            let text = character.to_string();
            let lower = js_to_locale_lower_case(&text, Some(locale)).expect("valid locale");
            if lower != text {
                writeln!(out, "{locale} {code:x}:{}", hex(&lower)).expect("write to a string");
            }
        }
        for text in contexts {
            let lower = js_to_locale_lower_case(text, Some(locale)).expect("valid locale");
            writeln!(out, "{locale} {}", stringify(&JsValue::String(lower)))
                .expect("write to a string");
        }
    }
    for tag in TAGS {
        match js_to_locale_lower_case("I\u{130}\u{307}", Some(tag)) {
            Ok(lower) => writeln!(out, "tag {}", stringify(&JsValue::String(lower))),
            Err(error) => writeln!(out, "tag RangeError: {}", error.message),
        }
        .expect("write to a string");
    }
    out
}

#[test]
fn locale_lowercase_matches_node() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    let contexts = contexts();
    let strings = |items: &[&str]| {
        JsValue::Array(
            items
                .iter()
                .map(|item| JsValue::String((*item).to_owned()))
                .collect(),
        )
    };
    let request = JsValue::Object({
        let mut object = spocky_contracts::js_value::JsObject::new();
        object.insert("locales", strings(LOCALES));
        object.insert("exhaustive", strings(EXHAUSTIVE));
        object.insert(
            "contexts",
            JsValue::Array(contexts.iter().cloned().map(JsValue::String).collect()),
        );
        object.insert("tags", strings(TAGS));
        object
    });
    let file = std::env::temp_dir().join(format!("spocky-locale-{}.json", std::process::id()));
    std::fs::write(&file, stringify(&request)).expect("write cases");
    let expected = support::run_node(&node, &dist, NODE_SCRIPT, &[file.display().to_string()]);
    std::fs::remove_file(&file).expect("remove cases");
    let actual = rust_lines(&contexts);
    let mut mismatches = Vec::new();
    let (mut want, mut got) = (expected.lines(), actual.lines());
    loop {
        match (want.next(), got.next()) {
            (None, None) => break,
            (wanted, found) if wanted == found => {}
            (wanted, found) => {
                mismatches.push(format!("node: {wanted:?}\n  rust: {found:?}"));
                if mismatches.len() >= 30 {
                    break;
                }
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "lines differ:\n{}",
        mismatches.join("\n")
    );
}

#[cfg(unix)]
#[test]
fn default_locale_matches_node() {
    let Some((node, _)) = support::pinned() else {
        return;
    };
    let script = r#"console.log(JSON.stringify([Intl.DateTimeFormat().resolvedOptions().locale, "Iİ̇".toLocaleLowerCase()]));"#;
    for environment in ENVIRONMENTS {
        let output = run_node_under(&node, environment, script);
        assert!(output.status.success(), "node failed under {environment:?}");
        let printed = parse(String::from_utf8_lossy(&output.stdout).trim()).expect("node output");
        let pair = printed.as_array().expect("pair");
        let variables: BTreeMap<&str, &str> = environment.iter().copied().collect();
        let tag = default_locale_from(|name| variables.get(name).map(|value| (*value).to_owned()));
        assert_eq!(
            pair[0].as_str(),
            Some(tag.as_str()),
            "default locale under {environment:?}"
        );
        let lower =
            js_to_locale_lower_case("I\u{130}\u{307}", Some(&tag)).expect("default tag is valid");
        assert_eq!(
            pair[1].as_str(),
            Some(lower.as_str()),
            "toLocaleLowerCase() under {environment:?}"
        );
    }
}

#[cfg(unix)]
const NAMES: [&str; 4] = ["LC_ALL", "LC_MESSAGES", "LC_CTYPE", "LANG"];
#[cfg(unix)]
const VALUES: [Option<&str>; 5] = [
    None,
    Some("tr_TR.UTF-8"),
    Some("C"),
    Some("de_DE"),
    Some(""),
];

/// Every mixture of the four variables over a value set: node on macOS and
/// Linux resolves the default locale from `LC_ALL`, then `LC_MESSAGES`, then
/// `LANG`, and ignores `LC_CTYPE`; the port must pick the same.
#[cfg(unix)]
#[test]
fn default_locale_mixed_environments_match_node() {
    let Some((node, _)) = support::pinned() else {
        return;
    };
    let combinations = VALUES.len().pow(4);
    let environments: Vec<Vec<(&str, &str)>> = (0..combinations)
        .map(|index| {
            let mut rest = index;
            NAMES
                .iter()
                .filter_map(|name| {
                    let value = VALUES[rest % VALUES.len()];
                    rest /= VALUES.len();
                    value.map(|value| (*name, value))
                })
                .collect()
        })
        .collect();
    let script = r"console.log(Intl.DateTimeFormat().resolvedOptions().locale)";
    let results: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = environments
            .chunks(environments.len().div_ceil(8))
            .map(|chunk| {
                let node = &node;
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|environment| {
                            let output = run_node_under(node, environment, script);
                            assert!(output.status.success(), "node failed under {environment:?}");
                            String::from_utf8_lossy(&output.stdout).into_owned()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("node runs"))
            .collect()
    });
    assert_eq!(results.len(), environments.len());
    for (environment, printed) in environments.iter().zip(&results) {
        let variables: BTreeMap<&str, &str> = environment.iter().copied().collect();
        let tag = default_locale_from(|name| variables.get(name).map(|value| (*value).to_owned()));
        assert_eq!(
            printed.as_str(),
            format!("{tag}\n"),
            "default locale under {environment:?}"
        );
    }
}

/// A repeated piece that is not a BCP 47 variant (`LANG=de_DE@euro_euro`):
/// node's `toLocaleLowerCase()` still works, but `Intl.DateTimeFormat()`,
/// `localeCompare`, and `toLocaleString` throw `RangeError: Internal error.
/// Icu error.`, while `default_locale` returns the tag with the duplicate
/// removed (DIV-006). A repeated variant (`@abcde_abcde`) is fine in node.
#[cfg(unix)]
#[test]
fn repeated_modifier_piece_divergence_is_pinned() {
    let Some((node, _)) = support::pinned() else {
        return;
    };
    let script = r#"
const attempt = (f) => { try { return String(f()); } catch (e) { return `${e.name}: ${e.message}`; } };
console.log(JSON.stringify([
  attempt(() => "I\u0130\u0307".toLocaleLowerCase()),
  attempt(() => Intl.DateTimeFormat().resolvedOptions().locale),
  attempt(() => "a".localeCompare("b")),
  attempt(() => (1).toLocaleString()),
]));"#;
    let run = |lang: &str| {
        let output = run_node_under(&node, &[("LANG", lang)], script);
        assert!(output.status.success());
        parse(String::from_utf8_lossy(&output.stdout).trim()).expect("node output")
    };
    let throws = "RangeError: Internal error. Icu error.";
    let printed = run("de_DE@euro_euro");
    let items = printed.as_array().expect("array");
    assert_eq!(items[0].as_str(), Some("ii\u{307}\u{307}"));
    for item in &items[1..] {
        assert_eq!(item.as_str(), Some(throws));
    }
    let tag = default_locale_from(|name| (name == "LANG").then(|| "de_DE@euro_euro".to_owned()));
    assert_eq!(tag, "de-DE-x-lvariant-euro");
    assert_eq!(
        js_to_locale_lower_case("I\u{130}\u{307}", Some(&tag)).as_deref(),
        Ok("ii\u{307}\u{307}")
    );
    let printed = run("tr_TR@euro_euro");
    let items = printed.as_array().expect("array");
    let tag = default_locale_from(|name| (name == "LANG").then(|| "tr_TR@euro_euro".to_owned()));
    assert_eq!(
        js_to_locale_lower_case("I\u{130}\u{307}", Some(&tag)).as_deref(),
        Ok(items[0].as_str().expect("string"))
    );
    let repeated_variant = run("de@abcde_abcde");
    for item in repeated_variant.as_array().expect("array") {
        assert!(!item.as_str().expect("string").starts_with("RangeError"));
    }
}

/// Node on Windows ignores the environment: its default locale is the user
/// locale, and `default_locale` must read the same one. Also run in the
/// workflow after `Set-Culture` to a second locale, in a fresh process.
#[cfg(windows)]
#[test]
fn default_locale_matches_node_on_windows() {
    let Some((node, _)) = support::pinned() else {
        return;
    };
    let script = r#"console.log(JSON.stringify([Intl.DateTimeFormat().resolvedOptions().locale, "I\u0130\u0307".toLocaleLowerCase()]));"#;
    let output = Command::new(&node)
        .args(["-e", script])
        .output()
        .expect("run pinned node");
    assert!(output.status.success());
    let printed = parse(String::from_utf8_lossy(&output.stdout).trim()).expect("node output");
    let pair = printed.as_array().expect("pair");
    let tag = spocky_contracts::locale::default_locale();
    assert_eq!(pair[0].as_str(), Some(tag.as_str()));
    let lower = js_to_locale_lower_case("I\u{130}\u{307}", Some(&tag)).expect("valid tag");
    assert_eq!(pair[1].as_str(), Some(lower.as_str()));
}
