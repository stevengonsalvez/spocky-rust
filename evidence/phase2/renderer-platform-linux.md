# Dioxus Linux renderer platform evidence

Task P2-RENDERER-LINUX-01. This is measured candidate evidence for the Dioxus
desktop shell on Linux x86_64. It is not renderer selection and not a parity
claim. Every difference below is recorded as measured. No mask, threshold, or
normalization is applied.

## Boundary

`scripts/phase2/renderer-platform-linux.sh --build-image` builds a derived image
from `scripts/phase2/renderer-platform-linux.Dockerfile`. The base is the pinned
`rust:1.94-bookworm` digest
`sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55`. The
build installs the apt packages, generates the `en_US.UTF-8` locale, installs the
Rust `1.94.0` toolchain that the repository `rust-toolchain.toml` pins (the base
ships `1.94.1`), and builds every locked dependency. This is the only networked
step. The image is pinned by its local image ID in
`scripts/phase2/renderer-platform-linux.image-id`
(`sha256:289345c3127809ad4ecbaa16e7730db7a108cf59b8a3c68f3d8be5888ce001d4`). A
local image ID is a content digest of the image config, not a registry digest.
`image-packages.txt` lists every installed package version.

`scripts/phase2/renderer-platform-linux.sh` then runs one disposable container
from that image with `--network none`, 2 CPUs, 3 GB memory, `--memory-swap 3g`,
and a 1,200 second bound, and refuses to run if the local image ID differs from
the pin. The repository, the two pinned browser PNGs, and the pinned browser
comparison JSON are mounted read-only. The run builds the pilot offline
(`cargo build --locked --offline`). An earlier version of the runner ran apt and
let rustup download `1.94.0` on every run. Those downloads are now baked into the
image.

The environment is Xvfb `1280x800x24` at 96 dpi, no window manager, a private
D-Bus session with the AT-SPI registry, `en_US.UTF-8`, `GTK_THEME=Adwaita:light`,
`GDK_SCALE=1`, DejaVu Sans, and the software-rendering settings
`WEBKIT_DISABLE_COMPOSITING_MODE=1` and `WEBKIT_DISABLE_DMABUF_RENDERER=1`.
Rust and Cargo are `1.94.0`. Xvfb is `2:21.1.7-3+deb12u13`, xdotool
`1:3.20160805.1-5`, ImageMagick `8:6.9.11.60+dfsg-1.6+deb12u13`, at-spi2-core
`2.46.0-5`, python3-pyatspi `2.46.0-2`.

The window is moved to `0,0` and sized to `1280x800`. After a 10 second settle
the root window is captured once. There is no retry and no second capture.

## Visual result

The candidate PNG is `1280x800`, SHA-256
`a5bede533dce1a6e70fe44f020918b2bc3a80496484802f7fc7578dcda191bfe`. It is not
in the pinned desktop set (`fad844b5...6480f`, `59709577...f4dc7d`), so exact
membership is false.

| baseline | different pixels | normalized RMSE | difference box |
|---|---|---|---|
| `original-desktop.png` | 91009 | 0.054315250557046856 | 0,0,1280,781 |
| `original-repeat-desktop.png` | 91009 | 0.05431467328743619 | 0,0,1280,781 |

The method is complete PNG SHA-256 equality plus
`sqrt(mean squared RGBA byte difference) / 255`. The candidate PNG,
`visual.json`, and `atspi.json` from the offline pinned-image run are
byte-identical to the files committed from the earlier run that downloaded its
packages (commits `f030cfa0` for `candidate.png`, `5f2c430d` for `visual.json`,
`27d8b80a` for `atspi.json`).

Observed cause of the largest difference: the Dioxus desktop host adds a GTK
menu bar (`Window`, `Edit`, `Help`) above the web view. In the screenshot the
web content starts about 24 pixels lower than in the baseline, so the visible
web area is shorter than 800 pixels. The pinned browser images have no menu
bar. The candidate is also a WebKitGTK
render, while the baselines are Chromium renders, so text rasterization
differs. This evidence does not separate those causes numerically.

## Accessibility result

`atspi.json` holds the complete AT-SPI tree before interaction, a per-key focus
trace, the Tab walk, and the baseline comparison.

- The tree exposes a `document web` named `Dioxus app`, the `Primary navigation`
  and `Open project` landmarks, and the same 12 sidebar and footer buttons the
  baseline records, in the same order and with the same names.
- The Tab walk returns to its first entry after 21 steps. The baseline cycle
  has 20 entries. `namesEqual` is false.
- Steps 1 to 12 equal the baseline names. Step 13 is the `document web`
  (`Dioxus app`) where the baseline has an unnamed focusable `div`. Step 14 is
  `Close menu`, equal. Steps 15 to 17 are unnamed `section` nodes whose AT-SPI
  text is `Add a project\nOpen a folder on your machine\n`,
  `Import session\nOpen a Claude Code, Codex or other session you started in a
  terminal\n`, and `Setup providers\nConfigure Claude Code, Codex, and more\n`.
  They carry the same words as the baseline `div` texts, with newlines where
  the baseline has spaces, so they are not byte-equal. Steps 18
  to 20 are `Star`, `Sponsor`, `Community`, equal.
- Step 21 leaves focus on no node, then the next Tab returns to the first
  button. The baseline cycle has no such step.
- AT-SPI roles use ATK vocabulary (`push button`, `section`) where the browser
  record uses ARIA tags and roles. Roles are stored raw and are not mapped.
- Focus inside the web view is reported by the deepest FOCUSED node. The
  `scroll pane` and `document web` ancestors stay FOCUSED throughout, so the
  first FOCUSED node is always the scroll pane. The checker therefore reads the
  last FOCUSED node in document order. The full trace is retained.

## Interaction result

- Plus control: Tab walk, then Return on `New workspace` (the first
  keyboard-focusable control, with the Plus icon). Focus stays on the button,
  no dialog appears, and the tree snapshot differs from the pre-interaction
  snapshot (`treeChanged` true). The pinned baseline records no activation of
  this control, so there is no baseline value to compare. This is recorded as
  an observation only.
- Add a project: Tab until the focused node text contains `Add a project`,
  then Return. A dialog is observed. Its three control names are `Search for
  directory Find a directory on isolated-baseline`, `Clone from GitHub Search
  projects available to your GitHub account`, and `New directory Create an
  empty directory on isolated-baseline`, equal to the pinned baseline dialog
  controls. The dialog is matched by AT-SPI role `dialog`, not by DOM text.

## Evidence files

| file | SHA-256 |
|---|---|
| `renderer-platform-linux/candidate.png` | `a5bede533dce1a6e70fe44f020918b2bc3a80496484802f7fc7578dcda191bfe` |
| `renderer-platform-linux/visual.json` | `39d85d24834f099cb8627004ba0ce069e4e8a78a26ac30a895d23ba43005dd7b` |
| `renderer-platform-linux/atspi.json` | `afdc728d2694f3c12d792e1ed8f80da64bb3129a708ef8ce2cccb7689365719c` |
| `renderer-platform-linux/container.log` | `a039117f9680b2b28ab281d39c94228e560988870da68b6bfcf0d19f2c311d49` |
| `renderer-platform-linux/container-state.json` | `9479a14a17c29cb7cf9e045506207eaf60670b53ba17ee6625f71e1fef983e60` |
| `renderer-platform-linux/image-packages.txt` | `a60bb8190c87f2cc47965ebc24a2dc187318cef27f1aef957d8130032209d981` |
| `renderer-platform-linux.image-id` (under `scripts/phase2/`) | `0be2aa5196ebb51395400e81f47274151bbb34f4d083c8ff23ea3d3552eadb2f` |

`application.log` is empty (SHA-256 `e3b0c442...b855`), so the application
wrote nothing to stdout or stderr. The container exited 0 and was not
OOM-killed. The run took about 45 seconds.

The debug binary SHA-256 (`45e108d2...ebc1` in this run) is not an evidence
key. It differed between earlier runs that used different Cargo home paths.
The cause of that difference is unverified. The difference box in
`visual.json` is computed on the RGB channels, because Pillow `getbbox` on RGBA
reads only alpha and returns an empty box.

Reproduce with:

```text
sh scripts/phase2/renderer-platform-linux.test.sh
scripts/phase2/renderer-platform-linux.sh --build-image
scripts/phase2/renderer-platform-linux.sh
```

## Open

Windows, macOS, Android, iOS, packaging, delivery, mobile viewport, and renderer
selection are out of scope. The Linux candidate does not match the pinned
visual baseline. The menu bar and the 21st focus step are unresolved Linux
differences. The Plus activation has no baseline record.
