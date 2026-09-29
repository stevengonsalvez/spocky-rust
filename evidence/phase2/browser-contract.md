# Browser contract pilot

Integrated pilot command:

```text
cargo run --quiet -p paseo-browser-pilot
```

Raw output is retained at `evidence/raw/phase2/browser-contract.json`.
It is 1,581 bytes with SHA-256
`e20892dad5bf06273032905bbd805ccd0b545b149272d580d40b349664c518ae`.

Eight targeted tests, formatting, and clippy pass. The executable trace covers
tab identity, locked webview ownership, trusted automation, downloads, deep
links, crash, restart, and unsupported trusted input.

This is a contract model. It has no embedded webview, network transfer, visual
capture, or platform launch evidence and cannot select a browser host.
