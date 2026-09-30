# Linux delivery runtime evidence

Task `P2-DELIVERY-01`, Linux slice. Baseline `paseo@5de45e208690b0efc51c59a585ae9729325a9204`.
This is a bounded lifecycle slice with real `dpkg` execution. It is not Linux delivery parity.

## Reproduce

```text
sh scripts/phase2/delivery-linux-runtime.test.sh
scripts/phase2/delivery-linux-runtime.sh --print-plan
scripts/phase2/delivery-linux-runtime.sh
```

The runner starts one container named `spocky-delivery-linux-<epoch>-<pid>` from
`rust@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55`
(Debian 12, dpkg 1.21.22, Linux 6.8 x86_64), repository mounted read-only, under a
1,500-second hard timeout. On timeout it removes only that container name. The stub-docker
test proves this path. Inside it runs `cargo clippy -D warnings`, the `linux_delivery_runtime`
tests, the report bin, and the ignored `linux_delivery_dpkg` test. The disposable-root guard
refuses the deb lane without `SPOCKY_DELIVERY_DISPOSABLE_ROOT=1`, without root, or when a
Paseo install already exists.

Retained run, from clean tree at commit `80b5bda1816b0c1f618a181fe648c2b699cb0bbd`,
container `spocky-delivery-linux-1790812637-6056`, exit status 0, no container left behind:

| artifact | SHA-256 |
|---|---|
| `delivery-linux-runtime/container.log` | `ca80b66d7f25e2faf58587a311abfa46e37e0814c1414c2507a03eb3e02f94b7` |
| `delivery-linux-runtime/run-metadata.json` | `31b4371386bbbd0d0d0004f657f8619716670170b5fed343482e324d9c592f53` |
| `delivery-linux-runtime/run/linux-delivery-report.json` | `e81f3644a06089dbed8835d8f6b0745914b0c517b67b0b407c4d8a691e7a5179` |

`container.log` lists the digest of every retained package and of the fixture trees.
`run/packages/` holds the three debs, the truncated deb, and the three AppImage fixtures.
The exact command, exit code, and stdout/stderr of each `dpkg` and launch step are in the report.

## Baseline sources

| Fixture | Source | SHA-256 |
|---|---|---|
| `launcher.sh` | `packages/desktop/scripts/linux-sandbox/launcher.sh` at the pin, byte-identical | `38cb6a3126e63405827a1836e40ea64324aa707d304c6bce2d4283fdc782c36a` |
| `after-install.tpl` | `packages/desktop/scripts/linux-sandbox/after-install.tpl` at the pin, byte-identical | `3afd8f459c65c1919b648d1cfbe618509b8fe00f01b6d15deab1c503e30d189e` |
| `after-remove.tpl` | `app-builder-lib@26.8.1` `templates/linux/after-remove.tpl` (the baseline sets no `afterRemove`) | `b8d4c31b037d4888fa2a9a99226bf09d0f3ed7724075675c490a38155667e403` |

The npm tarball integrity matched the baseline `package-lock.json` sha512 before extraction.
The `${executable}` and `${sanitizedProductName}` variables expand to `Paseo`, as in the baseline.

## Observed on Linux

Deb lane, real `dpkg` as root, launches as uid 1000 through `/usr/bin/Paseo`:

| Step | Observation |
|---|---|
| install | `update-alternatives` links `/usr/bin/Paseo` to `/etc/alternatives/Paseo` to `/opt/Paseo/Paseo`; `chrome-sandbox` is `0:0:4755` |
| launch | baseline launcher reports `[linux-sandbox] enabled: root-owned SUID helper available` (`unshare --user` returned EPERM in the container); `PASEO_HOME` state is read |
| truncated deb | `dpkg -i` exits 1 at `--control`; version, executable digest, and state digest unchanged |
| upgrade | `Unpacking paseo (1.1.0) over (1.0.0)`; launch reports 1.1.0 |
| rollback | `dpkg -i` of 1.0.0 prints `warning: downgrading`; executable digest returns to the 1.0.0 value |
| remove | status `rc`; `/opt/Paseo` gone; `update-alternatives` warns; `/usr/bin/Paseo` and `/etc/alternatives/Paseo` absent |
| purge | status not installed; `~/.paseo` state digest still equal to the install-time digest |

The baseline `after-remove` runs `update-alternatives --remove Paseo /usr/bin/Paseo`, which names a
path dpkg never registered (`/opt/Paseo/Paseo`). It is a no-op. dpkg's own cleanup of the vanished
alternative removes the links and prints two warnings. This is recorded as baseline behavior, not fixed.

AppImage lane, real files on the container disk, fixture payload, launched through the CLI link:

- The path stays `Paseo-x64.AppImage` and the `~/.local/bin/paseo` target never changes across
  install, update, and rollback (`cliLinkTarget` identical in every step).
- A payload whose size or sha512 disagrees with the manifest is rejected (`update size mismatch`),
  leaving install, retained file, and state untouched.
- Update retains the previous file, rollback swaps them, uninstall removes both, keeps `~/.paseo`,
  and leaves the user-owned CLI link dangling (`cliLinkResolves` false).

Findings while running on Linux:

- Hard-link retention returned the new content through the retained name on the Docker Desktop
  bind mount. The runtime now copies, and the report test asserts `retainedVersion`.
- The macOS delivery bin had no `main` on Linux and the local runtime bin had an unused import, so
  `cargo test` and clippy failed for the package in a container. Both were repaired (`e101ff4`).

## Remaining gaps

Not proven, so `P2-DELIVERY-01` stays open:

- Payloads are shell fixtures. No Electron build, real squashfs AppImage, or bundled CLI shim run.
- No RPM or tar.gz lane, and no Fedora dependency resolution.
- No user-namespace-enabled launch, AppArmor profile install, or restricted-host Ubuntu 24.04 case.
- No electron-updater download, `latest-linux.yml` from a real release, rollout admission,
  beta channel, or `quitAndInstall`. The AppImage manifest is fixture-shaped.
- No desktop entry, MIME, icon registration, deb `Depends`, signing, or repository metadata.
- Baseline has no rollback and no AppImage uninstall. Both are pilot semantics here.
- Package identifiers (`Paseo`, `/opt/Paseo`, `Paseo-x64.AppImage`, `~/.paseo`, `PASEO_HOME`,
  `APPIMAGE`, the `paseo` CLI name, deb package name `paseo`) are kept as compatibility
  identifiers. The deb package name is a fixture choice, not verified against electron-builder output.
  The Spocky rename of these needs rows in `porting/spocky-branding-inventory.md`, owned by the lead.
- x86_64 only. No Windows or ARM Linux delivery.
