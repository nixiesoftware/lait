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

The initial slice ports the primary Library window: its compact 640px geometry,
platform caption, Library rail, selected World hero, lifecycle action band,
people glance, and operational bar. Its canned projection in `src/client.ts`
is an explicitly temporary mirror of the Flutter `ClientView`; it exists until
the browser receives a generated contract from the existing Rust core.

## Run

```sh
npm install
npm run dev
```

`?platform=macos` and `?platform=windows` preview the Flutter client's existing
caption variants. Production does not infer that choice from a user agent: the
desktop shell supplies it and the kiosk/browser entry point uses the generic
profile.

## Design boundary

React Aria owns accessible interaction semantics. Astrolabe owns appearance and
platform variants. The application model must not branch by platform; only
semantic UI primitives may vary by `PlatformProfile`.
