# lait-net — a self-sovereign L3 tunnel (Linux-first prototype)

The first slice of lait's own **packet layer**: the thing that lets IP packets
between two members' devices ride lait's fabric instead of a Tailscale tunnel —
with **no coordination server** handing out addresses and **no identity
provider** deciding who you are.

## The one idea

**A device's address is derived from its identity.** A lait `DeviceId` *is* an
ed25519 key, and its tunnel address is a hash of that key in the ULA range
`fd00::/8`. No allocator, no authority, no collisions — the address *is* the
key. That is exactly the job Tailscale needs a coordinator for, done with a hash
(see `src/lib.rs::ula_from_key`).

## What this prototype is, and is not

- **Is**: a working point-to-point L3 tunnel on Linux. It opens a TUN interface,
  assigns the key-derived address, and carries real IP packets — enough to
  `ping6` a peer across it on a Raspberry Pi.
- **Is not, yet**: encrypted, or riding lait's real transport. The carry here is
  plain UDP between configured peers, on purpose: it isolates and proves the
  hard, OS-specific half (a TUN on real hardware) before that half folds into
  iroh and the `lait/exec/1` plane. **Run it on a trusted link.**

## Test it on a Raspberry Pi (and a second Linux box)

Both ends need Linux and `CAP_NET_ADMIN` (run with `sudo`). Build for the Pi
(aarch64) with a cross toolchain or `cargo build --release` on the Pi itself.

1. Pick a 32-byte seed for each node (64 hex chars), e.g.
   `head -c32 /dev/urandom | xxd -p -c32`.
2. Learn each node's public key and address without touching the network:
   ```sh
   lait-net --seed <A-seed-hex> --print   # prints A's pubkey and fd..:: address
   lait-net --seed <B-seed-hex> --print   # prints B's pubkey and fd..:: address
   ```
3. Bring both tunnels up, each pointing at the other's UDP endpoint and public
   key:
   ```sh
   # On A:
   sudo lait-net --seed <A-seed-hex> --listen 0.0.0.0:51820 \
                 --peer <B-host>:51820=<B-pubkey-hex>
   # On B (the Pi):
   sudo lait-net --seed <B-seed-hex> --listen 0.0.0.0:51820 \
                 --peer <A-host>:51820=<A-pubkey-hex>
   ```
4. From A, ping B's derived address (shown by `--print` on B):
   ```sh
   ping6 <B-address>
   ```
   Packets leave A's TUN, ride UDP to the Pi, enter the Pi's TUN, and the Pi's
   stack answers — an L3 tunnel addressed entirely by device key.

`--dev` names the interface (default `lait0`). Unknown-destination packets are
dropped.

## The ladder from here to the real packet layer

This crate is slice 1 of the L3 plan (Specs in the Netstack project). In order:

1. **Derived addressing + Linux TUN + packet carry** — *this crate*. ✅
2. **Carry over lait's transport** — replace UDP with an iroh endpoint, so the
   tunnel rides lait's own relays and NAT traversal (and is encrypted by QUIC).
3. **The `lait/exec/1` net plane** — carry packets as a declared plane inside the
   running daemon, keyed by peer, instead of a standalone binary.
4. **Authority → packet filter** — compile the Space's signed per-device consent
   into a per-host filter: which peer may reach which port. This is what keeps
   L3 from meaning "any member reaches everything".
5. **MagicDNS-equivalent** — resolve address-book petnames to derived addresses.
6. **macOS (utun + Network Extension entitlement) and Windows (WinTun + a
   privileged service)** — the same seam as `src/tun.rs`, behind the gated /
   privileged installs. File the Apple entitlement early; it is the only step
   that waits on someone else.
7. **Subnet routers / exit nodes** — full-Tailscale reach, last.
