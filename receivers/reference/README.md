# Astrolabe Display reference receiver

This executable is the protocol-major-1 receiver oracle for a real self-hosted
Astrolabe coordinator. It does not contain a demo playlist and does not ask
Astrolabe to render a World. It pairs as a device, accepts a compiled bounded
display program, verifies and stages the program's frame assets, advances the
program at item boundaries, publishes the current presentation atomically, and
reports bounded health.

The bootstrap file is non-secret trust material copied from the coordinator:

```json
{
  "protocol_major": 1,
  "trust": {
    "kind": "pinned_certificate",
    "origin": "https://192.0.2.10:7443",
    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
  },
  "rendezvous": null
}
```

Run it with receiver-owned state and presentation directories:

```text
cargo run -p astrolabe-display-reference -- \
  --bootstrap bootstrap.json \
  --state .astrolabe-display \
  --output display-output
```

The receiver displays the fingerprint and six-word confirmation phrase before
it polls for approval. Confirm the same values in Astrolabe and type `yes` at
the local display. Credentials are written through the Mechanics private-secret
boundary (DPAPI-bound on Windows and owner-only on Unix). The bootstrap pin is
checked during TLS before any pairing or credential-bearing request; redirects
are disabled.

`display-output/active.json` is the atomic native-renderer handoff. When its
scene is `frame`, `display-output/frame.png` contains the fully verified current
frame. A native shell can watch the status file and swap the image on its own
compositor without implementing the network protocol.
