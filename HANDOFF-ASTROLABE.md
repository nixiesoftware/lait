# Astrolabe

The local client through which a person reaches the Worlds their device serves.
It is a library, a launcher, and an identity — it **never draws a World**.
`Open` hands the World to the person's browser; `products/issues` stays the
authority on its own presentation.

The earlier `HANDOFF-ASTROLABE-2.md` / `-3.md` pages described the retired
egui interface and are gone. Current facts live here and in
[`CLAUDE.md`](./CLAUDE.md). Plans live as Specs in the tracker, not in
markdown.

## What to run

```sh
cd apps/astrolabe-web
npm ci && npm run tauri dev   # canonical Tauri interface + same-tree Rust host
npm run check && npm test     # TypeScript contract and interface tests
cd ../..
cargo test -p astrolabe       # core: client/model/runtime + launch + packaging
```

The Rust crate (`tools/astrolabe`) is the core. Tauri
(`apps/astrolabe-web`, React/TypeScript over `src-tauri`) draws a `ClientView`
and dispatches `ActionRequest`. There is one model of client state; the
interface holds nothing but drafts. Flutter (`apps/astrolabe`) is deprecated,
unwired, and does not build or ship.

Kill `astrolabe.exe` before the Rust suite: the runner holds the
single-instance mutex.

## MCP

Astrolabe authors the editor binding. It does not parent `lait mcp`.

- The World designs the agent surface (tools, omissions, teaching text).
- Astrolabe writes `command: lait`, `args: ["mcp"]`, `LAIT_AGENT`, and
  `LAIT_WORLD` from the selected Library row.
- Traffic is editor → `lait mcp` → daemon → WorldHost.

A project-scoped binding lands beside a `.lait` store, never inside one.
An unknown client name is refused, not signed as Claude. The binding is
authored for the selected Library World (`LAIT_WORLD`).

See [`docs/AGENT-EXPERIENCE.md`](./docs/AGENT-EXPERIENCE.md) for sponsor +
attach, and [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) for the
ownership split.
