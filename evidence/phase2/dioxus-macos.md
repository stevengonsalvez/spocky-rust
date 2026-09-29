# Dioxus macOS release pilot

Dioxus `0.7.0` produced and launched a frozen unsigned macOS release bundle on
2026-09-30. This is candidate evidence, not renderer selection or parity evidence.

## Build and bundle

```text
.tools/bin/dx build --macos -p paseo-ui-renderer-pilot \
  --bin paseo-ui-desktop --no-default-features --features desktop \
  --bundle macos --release --codesign false --frozen --verbose
```

The bundle path was
`target/dx/paseo-ui-desktop/release/macos/PaseoUiDesktop.app`.
`codesign` reported no signature, and `spctl` rejected the bundle because no
usable signature exists. This was expected for the explicit unsigned pilot.

| File | Bytes | SHA-256 |
|---|---:|---|
| `Contents/MacOS/paseo-ui-desktop` | 5,120,740 | `e7d2d0ea9aef03d66b187df836c37e4cfb07f39af06dd6c49c1d6603e7ab807e` |
| `Contents/Info.plist` | 927 | `c6e03f434d677e5c1949f91a6cdf55e7b331652a7870a1059d1aed659de61f39` |

## Launch

The executable ran in exact tmux session
`dev-paseo-ui-macos-release-1790726000`. Launch Services registered PID 43845 as
`PaseoUiDesktop` with bundle identifier `com.example.PaseoUiDesktop`. The process
remained alive for 37 seconds with no stderr, stdout, or crash. It was stopped by
exact PID, then the exact tmux session was stopped.

The retained ignored log is zero bytes with SHA-256
`e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`.
No screenshot or accessibility tree could be captured because the controlling
Orca host lacks macOS Screen Recording and Accessibility access. Signed packaging,
installation, update, rollback, and pinned-baseline behavior remain unproven.
