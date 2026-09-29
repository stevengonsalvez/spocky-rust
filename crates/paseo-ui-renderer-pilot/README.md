# Dioxus UI renderer pilot

Temporary nested workspace testing Dioxus 0.7 against one interactive Paseo shell.
It is independent from the parent Cargo workspace.

The shell uses one RSX component across three feature-separated binaries:

- `web`: browser DOM through `dioxus/web`
- `desktop`: system WebView through `dioxus/desktop`
- `mobile`: system WebView through `dioxus/mobile`

Buttons use native HTML interaction and update signal-backed selected-agent state.
The host-only SSR path provides deterministic DOM and accessibility-contract tests.

See [`feasibility.json`](feasibility.json) for commands, results, limits, and the
selection decision. Passing a Cargo check is compile evidence only. It does not
prove launch, visuals, screen-reader behavior, native integration, or packaging.

## Sources

- [Dioxus 0.7 platform support](https://dioxuslabs.com/learn/0.7/guides/platforms/)
- [Dioxus 0.7 desktop](https://dioxuslabs.com/learn/0.7/guides/platforms/desktop/)
- [Dioxus 0.7 mobile](https://dioxuslabs.com/learn/0.7/guides/platforms/mobile/)
- [Dioxus 0.7 accessibility guidance](https://dioxuslabs.com/learn/0.7/tutorial/next_steps/)
