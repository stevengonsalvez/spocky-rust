//! `assertAbsolutePath` and `isSameOrDescendantPath` against the pinned
//! `server/path-utils.js` over POSIX, Windows, drive-letter and separator
//! edge cases.

mod support;

use spocky_contracts::js_value::{JsValue, stringify};
use spocky_terminal::path_utils::{assert_absolute_path, is_same_or_descendant_path};

const PATHS: &[&str] = &[
    "",
    "/",
    "//",
    "/a",
    "/a/",
    "/a/b",
    "/ab",
    "/a b",
    "a",
    "a/b",
    "./a",
    "~",
    "\\",
    "\\a",
    "\\\\server\\share",
    "C:",
    "C:/",
    "C:\\",
    "C:\\a",
    "c:/a",
    "c:/A/b",
    "C:/a/",
    "C:/a/b",
    "C:a",
    "1:/a",
    "cc:/a",
    "\u{e9}:/a",
    "\u{1f600}",
    "/\u{1f600}",
    "/A",
    "/a/\\b",
    "D:\\a\\b",
];

const SCRIPT: &str = r"
const [terminalDir, pathsJson] = process.argv.slice(1);
const { assertAbsolutePath, isSameOrDescendantPath } =
  await import(`${terminalDir}/../server/path-utils.js`);
const paths = JSON.parse(pathsJson);
const absolute = paths.map((path) => {
  try { assertAbsolutePath(path); return null; } catch (error) { return error.message; }
});
const pairs = paths.map((base) => paths.map((candidate) => isSameOrDescendantPath(base, candidate)));
process.stdout.write(JSON.stringify({ absolute, pairs }));
";

#[test]
fn path_checks_match_the_pinned_build() {
    let Some(pinned) = support::pinned("path-utils differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let input = stringify(&JsValue::Array(
        PATHS
            .iter()
            .map(|path| JsValue::String((*path).to_owned()))
            .collect(),
    ));
    let expected = support::run_node(&pinned, SCRIPT, &[&input]);

    let absolute: Vec<JsValue> = PATHS
        .iter()
        .map(|path| match assert_absolute_path(path) {
            Ok(()) => JsValue::Null,
            Err(message) => JsValue::String(message.to_owned()),
        })
        .collect();
    let pairs: Vec<JsValue> = PATHS
        .iter()
        .map(|base| {
            JsValue::Array(
                PATHS
                    .iter()
                    .map(|candidate| JsValue::Bool(is_same_or_descendant_path(base, candidate)))
                    .collect(),
            )
        })
        .collect();
    let mut object = spocky_contracts::js_value::JsObject::new();
    object.insert("absolute", JsValue::Array(absolute));
    object.insert("pairs", JsValue::Array(pairs));
    assert_eq!(stringify(&JsValue::Object(object)), expected);
}
