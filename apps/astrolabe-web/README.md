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

The port covers the Flutter client's surfaces whole:

- **Library** (`src/app.tsx`) — the rail with World marks and artwork heroes,
  the lifecycle action band, the people glance, the operational bar, and the
  Present-here control. Window controls are the OS header's own; closing the
  primary window hides it to the tray, where Quit is the one act that stops
  the client.
- **Address book** (`src/book.tsx`) — the portrait rolodex: canonical card,
  presence-parted list, search, the profile subsurface, plus the Messages
  section and the Incoming door into correspondence.
- **Chat** (`src/chat.tsx`) — conversations, never an inbox: tab chrome,
  grouped transcript with day pills and quiet dividers, per-kind message
  components, the composer.
- **Displays** (`src/displays.tsx`) — the coordinator, pairing approval, and
  the full assignment dialog.
- **Big Picture** (`src/present.tsx`) — this machine as a screen: the chooser,
  frame pacing, source/delivery banners, durable fullscreen (taken inside the
  entering gesture, watched, retakeable in a browser) and a screen wake lock.
- **World settings** (`src/settings.tsx`) — a read-only snapshot carried in
  its own window's URL.

## Client bridge

`src/client.ts` defines the exact client seam: whole `ClientView` snapshots
arrive through `current()` and `watch()`, and `dispatch()` returns the
immediate post-request snapshot. The host must inject
`window.__ASTROLABE_CLIENT__` before loading the bundle.

Development uses a stateful fixture transport that follows the same protocol.
Production deliberately reports a missing host bridge rather than presenting
fixture data as a local identity.

The included Tauri host in `src-tauri/` is that production bridge. It starts
the existing `tools/astrolabe` core, calls its existing `current` and
`dispatch` functions, and relays its native subscriber stream as a WebView
event. It also owns what only a host can: the owned windows (book, displays,
chat, per-World settings), window fullscreen, the tray, and the macOS
application menu. World artwork crosses through its own `world_artwork`
command — a build constant, asked once per mount and cached.

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
