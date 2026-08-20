# Astrolabe web

The browser-first Astrolabe client. It will replace the Flutter interface when
it reaches feature parity, but is deliberately independent while it is being
developed. The port is fidelity-first: it follows the current Flutter client
and does not use Workbench as its primary product surface.

Astrolabe has two backend modes:

- **ordinary** — one local `lait` identity through the host, Space, and World
  HTTP planes;
- **workbench** — local development supervision through `lait-workbench`'s
  authenticated loopback API.

The initial slice ports the primary Library window: platform caption, Library
rail, selected World hero, lifecycle action band, people glance, and operational
bar. It fills its host; native window size remains an OS-shell concern.

## Client bridge

`src/client.ts` defines the exact primary-window seam: whole `ClientView`
snapshots arrive through `current()` and `watch()`, and `dispatch()` returns
the immediate post-request snapshot. The host must inject
`window.__ASTROLABE_CLIENT__` before loading the bundle. It carries only the
actions this surface currently exposes: refresh, open, update a World, and
stop a head.

Development uses a stateful fixture transport that follows the same protocol.
Production deliberately reports a missing host bridge rather than presenting
fixture data as a local identity.

The included Tauri host in `src-tauri/` is that production bridge. It starts
the existing `tools/astrolabe` core, calls its existing `current` and
`dispatch` functions, and relays its native subscriber stream as a WebView
event. Its JSON DTO deliberately contains only the projection the primary
Library window renders.

## Run

```sh
npm install
npm run dev
```

To run the desktop host (after its Rust dependencies have built):

```sh
npm run tauri dev
```

`?platform=macos` and `?platform=windows` preview the Flutter client's existing
caption variants. Production does not infer that choice from a user agent: the
desktop shell supplies it and the kiosk/browser entry point uses the generic
profile.

## Design boundary

React Aria owns accessible interaction semantics. Astrolabe owns appearance and
platform variants. The application model must not branch by platform; only
semantic UI primitives may vary by `PlatformProfile`.
