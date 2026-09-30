# Browser contract pilot

Integrated pilot command:

```text
cargo run --quiet -p paseo-browser-pilot
```

Raw output is retained at `evidence/raw/phase2/browser-contract.json`.
It is 1,581 bytes with SHA-256
`e20892dad5bf06273032905bbd805ccd0b545b149272d580d40b349664c518ae`.

Ten targeted tests and clippy pass. Eight contract tests cover tab identity,
locked webview ownership, trusted automation, downloads, deep links, crash,
restart, and unsupported trusted input. Two macOS runtime tests launch a real
WKWebView and exercise rendered UI, host-scoped navigation, queued deep links,
network download, injected process failure, checkpoint recovery, and restart.

The macOS runtime is still a pilot. It has no production host wiring,
compositor capture, exact pixel evidence, or Windows, Linux, iOS, and Android
host execution. It cannot select the ecosystem browser host yet.
