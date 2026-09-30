# Dioxus Linux runtime pilot

Dioxus `0.7.0` built and launched the desktop candidate on Linux x86_64 under
disposable Xvfb. This is candidate runtime evidence, not renderer selection or
parity evidence.

## Boundary

`scripts/phase2/dioxus-linux-runtime.sh` mounts the Rust repository read-only in
the pinned `rust:1.94-bookworm` image digest
`sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55`.
The command has a 1,200-second bound. Cargo output and the launched executable
remain inside the disposable container. Docker removes the container after the
run.

The captured environment used Rust `1.94.0`, Cargo `1.94.0`, GTK `3.24.38`,
WebKitGTK `2.50.6`, and Xvfb `21.1.7`. The locked debug binary SHA-256 was
`8643c6108b267679af3ffc41d64c45b44a33b45c5a0cff3ef50434694c0e77e4`.

## Launch result

The executable stayed alive for 10 seconds under Xvfb without stdout or stderr.
The harness stopped its exact PID and verified the empty application log. No
container remained after the run.

Reproduce with:

```text
sh scripts/phase2/dioxus-linux-runtime.test.sh
scripts/phase2/dioxus-linux-runtime.sh
```

The raw log is 22,660 bytes with SHA-256
`9623a652c785e1dd88cb562e0630ec7593f378ec56282c7868b7f72efb109911`.

This proves Linux x86_64 compilation and bounded headless launch only. Linux
visual, accessibility, interaction, packaging, installation, update, rollback,
and pinned-original comparison remain open.
