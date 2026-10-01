# Dioxus Linux renderer platform evidence

Task P2-RENDERER-LINUX-01. This is measured candidate evidence for the Dioxus
desktop shell on Linux x86_64. It is not renderer selection and not a parity
claim. Every difference below is recorded as measured. No mask, threshold, or
normalization is applied.

## Boundary

`scripts/phase2/renderer-platform-linux.sh` runs one disposable container from
the pinned `rust:1.94-bookworm` digest
`sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55`
with 2 CPUs, 3 GB memory, and a 1,200 second bound. The repository, the two
pinned browser PNGs, and the pinned browser comparison JSON are mounted
read-only. Cargo output lives in two named Docker volumes
(`spocky-renderer-linux-target`, `spocky-renderer-linux-cargo-home`) because
the Colima VM shares only `$HOME` with macOS.

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
`sqrt(mean squared RGBA byte difference) / 255`. The candidate hash was
identical in every run that reached capture after the rendering settings were
added (runs 3, 5, 6, 7, and 8).

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
| `renderer-platform-linux/container.log` | `188ed10fa3a20b5755ebf01ae2a4433cd80b1cf16b968af10681181d8ac97fa7` |
| `renderer-platform-linux/container-state.json` | `1555babfd1ef08612f09adf5fe162540bfe12b197bf732c6d000a51ded1fa246` |

`application.log` is empty (SHA-256 `e3b0c442...b855`), so the application
wrote nothing to stdout or stderr. The container exited 0 and was not
OOM-killed.

The debug binary SHA-256 differed between runs, so it is not an evidence key.
The cause is unverified. Runs before the final one recorded an empty difference
box because of a defect in the comparison script (Pillow `getbbox` on RGBA
reads only alpha). That defect is fixed and the table above comes from the
final run.

Reproduce with:

```text
sh scripts/phase2/renderer-platform-linux.test.sh
scripts/phase2/renderer-platform-linux.sh
```

## Open

Windows, macOS, Android, iOS, packaging, delivery, mobile viewport, and renderer
selection are out of scope. The Linux candidate does not match the pinned
visual baseline. The menu bar and the 21st focus step are unresolved Linux
differences. The Plus activation has no baseline record.
