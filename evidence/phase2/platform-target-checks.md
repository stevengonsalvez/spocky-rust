# Platform target compile checks

Captured on 2026-09-29 from commit
`23124fa2cc80b4d4b13c86f976a62f80b8c14e22` with repository-local Rust 1.94.0.

The following command passed for `paseo-platform-bridge` and `paseo-ui-pilot`:

```text
cargo +1.94.0 check -p paseo-platform-bridge -p paseo-ui-pilot --target <target>
```

| Target | Result |
|---|---|
| `aarch64-apple-ios` | pass |
| `aarch64-linux-android` | pass |
| `wasm32-unknown-unknown` | pass |
| `x86_64-pc-windows-msvc` | pass |
| `x86_64-unknown-linux-gnu` | pass |
| `x86_64-apple-darwin` | pass |

These are compile checks. They do not satisfy launch, visual, accessibility,
native adapter, packaging, or update evidence requirements.
