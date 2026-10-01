//! End-to-end spike of the Rust `PGlite` host. Prints a JSON report.
//!
//! Usage: `spike <package root> <fresh data directory> <copy of a Node-made
//! data directory>`. The host runs on a dedicated large-stack thread.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spocky_pglite_host::package::{PinnedPackage, hex};
use spocky_pglite_host::pglite::{Compiled, EngineOptions, HostError, Pglite};
use spocky_pglite_host::protocol::{Backend, BindValue};

const THREAD_STACK: usize = 256 * 1024 * 1024;

fn rows(messages: &[Backend]) -> Vec<Vec<Option<String>>> {
    messages
        .iter()
        .filter_map(|message| match message {
            Backend::DataRow(cells) => Some(cells.clone()),
            _ => None,
        })
        .collect()
}

fn notices(messages: &[Backend]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Backend::Notice(fields) => fields.message.clone(),
            _ => None,
        })
        .collect()
}

fn walk(root: &Path, relative: &Path, output: &mut Vec<Value>) {
    let mut names: Vec<PathBuf> = fs::read_dir(root.join(relative))
        .expect("read directory")
        .map(|entry| PathBuf::from(entry.expect("entry").file_name()))
        .collect();
    names.sort();
    for name in names {
        let path = relative.join(&name);
        let metadata = fs::symlink_metadata(root.join(&path)).expect("metadata");
        let modified = metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        let mode = std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o7777;
        let mut entry = json!({
            "path": path.to_string_lossy(),
            "type": if metadata.is_dir() { "dir" } else if metadata.is_file() { "file" } else { "other" },
            "size": if metadata.is_file() { json!(metadata.len()) } else { Value::Null },
            "mode": format!("{mode:o}"),
            "mtimeMs": modified,
        });
        if metadata.is_file() {
            let bytes = fs::read(root.join(&path)).expect("read file");
            entry["sha256"] = json!(hex(&Sha256::digest(&bytes)));
        }
        output.push(entry);
        if metadata.is_dir() {
            walk(root, &path, output);
        }
    }
}

fn error_text(error: &HostError) -> String {
    format!("{error}")
}

#[allow(clippy::too_many_lines, reason = "one sequential spike workload")]
fn run(package_root: &Path, fresh: &Path, node_copy: &Path) -> Value {
    let mut steps = Vec::new();
    let started = Instant::now();
    let package = Arc::new(PinnedPackage::load(package_root).expect("pinned package"));
    let compiled = Compiled::new(package, &EngineOptions::default()).expect("compile");
    steps.push(json!({"step": "compile", "milliseconds": started.elapsed().as_millis()}));

    let opened = Instant::now();
    let mut database = match Pglite::open(&compiled, fresh) {
        Ok(database) => database,
        Err(error) => {
            steps.push(json!({"step": "open", "ok": false, "error": error_text(&error)}));
            return json!({"host": "rust", "steps": steps});
        }
    };
    steps.push(json!({
        "step": "open",
        "ok": true,
        "milliseconds": opened.elapsed().as_millis(),
        "initdb": database.initdb_report.as_ref().map(|report| json!({
            "exitCode": report.exit_code,
            "stderr": report.stderr,
            "entries": report.entries,
        })),
    }));
    match database.query_messages("select 1 as one", &[]) {
        Ok(messages) => steps.push(json!({"step": "select1", "rows": rows(&messages)})),
        Err(error) => steps.push(json!({"step": "select1", "error": error_text(&error)})),
    }
    let before = database
        .counters()
        .get("emscripten_throw_longjmp")
        .copied()
        .unwrap_or(0);
    match database.exec_messages(
        "\n  do $$\n  begin\n    perform 1 / 0;\n  exception when division_by_zero then\n    raise notice 'caught %', sqlstate;\n  end\n  $$;\n",
    ) {
        Ok(messages) => {
            let after = database.counters().get("emscripten_throw_longjmp").copied().unwrap_or(0);
            steps.push(json!({
                "step": "plpgsqlException",
                "longjmpCalls": after - before,
                "notices": notices(&messages),
            }));
        }
        Err(error) => steps.push(json!({"step": "plpgsqlException", "error": error_text(&error)})),
    }
    match database.query_messages("select * from missing_table", &[]) {
        Ok(_) => {
            steps.push(json!({"step": "structuredError", "error": "query unexpectedly succeeded"}));
        }
        Err(HostError::Database(fields)) => steps.push(json!({
            "step": "structuredError",
            "code": fields.code,
            "message": fields.message,
            "severity": fields.severity,
        })),
        Err(error) => steps.push(json!({"step": "structuredError", "error": error_text(&error)})),
    }
    let marker = database
        .exec_messages("create table spike_marker (id integer primary key, note text)")
        .and_then(|_| {
            database.query_messages(
                "insert into spike_marker values ($1, $2)",
                &[
                    BindValue::Text("1".into()),
                    BindValue::Text("spike marker".into()),
                ],
            )
        });
    steps.push(json!({"step": "marker", "ok": marker.is_ok(), "error": marker.err().map(|error| error_text(&error))}));
    let settings = database.query_messages(
        "select current_setting('TimeZone') as timezone, current_setting('server_version') as version",
        &[],
    );
    match settings {
        Ok(messages) => steps.push(json!({"step": "settings", "rows": rows(&messages)})),
        Err(error) => steps.push(json!({"step": "settings", "error": error_text(&error)})),
    }
    let closed = database.close();
    steps.push(json!({"step": "close", "ok": closed.is_ok(), "error": closed.err().map(|error| error_text(&error))}));
    let counters: Value = database
        .counters()
        .into_iter()
        .map(|(name, count)| (name.to_owned(), json!(count)))
        .collect::<serde_json::Map<String, Value>>()
        .into();

    let mut tree = Vec::new();
    walk(fresh, Path::new(""), &mut tree);

    let mut reopened = Vec::new();
    for (label, directory) in [("rustReopen", fresh), ("nodeDirectoryReopen", node_copy)] {
        match Pglite::open(&compiled, directory) {
            Ok(mut database) => {
                let query = if label == "rustReopen" {
                    "select 1 as one"
                } else {
                    "select id, note from spike_marker order by id"
                };
                let result = database.query_messages(query, &[]);
                let closed = database.close();
                reopened.push(json!({
                    "step": label,
                    "rows": result.as_ref().ok().map(|messages| rows(messages)),
                    "error": result.err().map(|error| error_text(&error)),
                    "closed": closed.is_ok(),
                }));
            }
            Err(error) => reopened.push(json!({"step": label, "error": error_text(&error)})),
        }
    }
    let mut node_tree_after = Vec::new();
    walk(node_copy, Path::new(""), &mut node_tree_after);
    json!({
        "host": "rust",
        "steps": steps,
        "counters": counters,
        "tree": tree,
        "reopen": reopened,
        "nodeCopyTreeAfterReopen": node_tree_after,
    })
}

fn main() {
    let arguments: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [package_root, fresh, node_copy] = arguments.as_slice() else {
        eprintln!("usage: spike <package root> <fresh data directory> <node data copy>");
        std::process::exit(2);
    };
    let (package_root, fresh, node_copy) = (package_root.clone(), fresh.clone(), node_copy.clone());
    let report = std::thread::Builder::new()
        .name("pglite-host".into())
        .stack_size(THREAD_STACK)
        .spawn(move || run(&package_root, &fresh, &node_copy))
        .expect("spawn host thread")
        .join()
        .expect("host thread");
    println!("{}", serde_json::to_string_pretty(&report).expect("json"));
}
