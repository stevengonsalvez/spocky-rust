//! `Array.prototype.sort` differential: V8's `TimSort` in the pinned Node
//! against `js_sort_by`, over comparators that are consistent (`t` only,
//! stable) and ones that are not (`NaN` differences fall through to a
//! tiebreak, as in `Date.parse(a) - Date.parse(b) || a.id.localeCompare(b.id)`
//! with an unparseable date). The whole sorted order is compared, not only
//! its first element, so the run detection, binary insertion, galloping, and
//! both merge directions all have to agree.

#[path = "support/pinned_node.rs"]
mod support;

use spocky_contracts::js::js_sort_by;

const NODE_SCRIPT: &str = r#"
const [dist, file] = process.argv.slice(1);
const { readFileSync } = await import("node:fs");
const out = [];
for (const [kind, rows] of JSON.parse(readFileSync(file, "utf8"))) {
  const items = rows.map(([t, k]) => ({ t: t === null ? NaN : typeof t === "string" ? Number(t) : t, k }));
  const compare = kind === "tiebreak" ? (a, b) => a.t - b.t || a.k - b.k : (a, b) => a.t - b.t;
  items.sort(compare);
  out.push(items.map((item) => item.k).join(","));
}
process.stdout.write(out.join("\n") + "\n");
"#;

/// xorshift64*, so every run builds the same arrays.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

type Row = (Option<f64>, i64);

/// `t` is `None` for `NaN`; `k` is distinct within an array.
fn pattern(random: &mut Random, shape: usize) -> Vec<Row> {
    let below = |random: &mut Random, bound: u64| -> f64 {
        f64::from(u32::try_from(random.below(bound)).expect("small bound"))
    };
    let length = match shape {
        0 => 2 + random.below(13),
        1 | 5 => 2 + random.below(300),
        2 | 3 => 20 + random.below(380),
        _ => 2 + random.below(200),
    };
    let mut times: Vec<Option<f64>> = Vec::new();
    match shape {
        0 => {
            while times.len() < usize::try_from(length).expect("length") {
                let pick = random.below(5);
                times.push((pick != 0).then(|| f64::from(u32::try_from(pick).expect("pick"))));
            }
        }
        2 | 3 => {
            while times.len() < usize::try_from(length).expect("length") {
                let run = 3 + random.below(38);
                let offset = below(random, 200);
                let mut block: Vec<f64> = (0..run)
                    .map(|step| {
                        offset + below(random, 3) + f64::from(u32::try_from(step).expect("step"))
                    })
                    .collect();
                if shape == 3 {
                    block.reverse();
                }
                times.extend(block.into_iter().map(Some));
            }
            times.truncate(usize::try_from(length).expect("length"));
        }
        4 => {
            for _ in 0..length {
                times.push((random.below(10) != 0).then(|| below(random, 40)));
            }
        }
        5 => times = vec![Some(7.0); usize::try_from(length).expect("length")],
        6 => {
            // Signed zeros and infinities: `-0 - 0` is 0, `Infinity - Infinity` is NaN.
            const PALETTE: [Option<f64>; 7] = [
                Some(-0.0),
                Some(0.0),
                Some(1.0),
                Some(-1.0),
                Some(f64::INFINITY),
                Some(f64::NEG_INFINITY),
                None,
            ];
            for _ in 0..length {
                times.push(PALETTE[usize::try_from(random.below(7)).expect("index")]);
            }
        }
        _ => {
            for _ in 0..length {
                times.push(Some(below(random, 1000)));
            }
        }
    }
    let mut keys: Vec<i64> = (0..i64::try_from(times.len()).expect("length")).collect();
    for index in (1..keys.len()).rev() {
        let other =
            usize::try_from(random.below(u64::try_from(index + 1).expect("index"))).expect("index");
        keys.swap(index, other);
    }
    times.into_iter().zip(keys).collect()
}

fn cases() -> Vec<(&'static str, Vec<Row>)> {
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut cases = Vec::new();
    for shape in 0..7 {
        for _ in 0..40 {
            let rows = pattern(&mut random, shape);
            cases.push(("tiebreak", rows.clone()));
            cases.push(("time", rows));
        }
    }
    cases
}

fn to_json(cases: &[(&str, Vec<Row>)]) -> String {
    let rows = cases.iter().map(|(kind, rows)| {
        let rows = rows
            .iter()
            .map(|(time, key)| match time {
                Some(time) if time.is_infinite() => {
                    let name = if *time > 0.0 { "Infinity" } else { "-Infinity" };
                    format!("[\"{name}\",{key}]")
                }
                Some(time) => format!("[{time:?},{key}]"),
                None => format!("[null,{key}]"),
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(r#"["{kind}",[{rows}]]"#)
    });
    format!("[{}]", rows.collect::<Vec<_>>().join(","))
}

fn rust_output(cases: &[(&str, Vec<Row>)]) -> String {
    let mut out = String::new();
    for (kind, rows) in cases {
        let mut items = rows.clone();
        let tiebreak = *kind == "tiebreak";
        js_sort_by(&mut items, |left, right| {
            let difference = left.0.unwrap_or(f64::NAN) - right.0.unwrap_or(f64::NAN);
            // `difference || left.k - right.k`: NaN and zero are falsy.
            if difference != 0.0 && !difference.is_nan() || !tiebreak {
                difference
            } else {
                f64::from(i32::try_from(left.1 - right.1).expect("small key"))
            }
        });
        let keys: Vec<String> = items.iter().map(|item| item.1.to_string()).collect();
        out.push_str(&keys.join(","));
        out.push('\n');
    }
    out
}

#[test]
fn js_sort_matches_v8_timsort() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    let cases = cases();
    let file = std::env::temp_dir().join(format!("spocky-js-sort-{}.json", std::process::id()));
    std::fs::write(&file, to_json(&cases)).expect("write cases");
    let expected = support::run_node(&node, &dist, NODE_SCRIPT, &[file.display().to_string()]);
    std::fs::remove_file(&file).expect("remove cases");
    let actual = rust_output(&cases);
    for (index, (node_line, rust_line)) in expected.lines().zip(actual.lines()).enumerate() {
        assert_eq!(
            rust_line, node_line,
            "case {index} ({}) differs",
            cases[index].0
        );
    }
    assert_eq!(actual, expected);
}
