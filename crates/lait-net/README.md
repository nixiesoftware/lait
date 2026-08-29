# lait-net — bring up lait's L3 tunnel (thin front over `netstack`)

`lait-net` is the **runnable front**; the logic lives in `crates/netstack`, the
sealed L3 boundary (addressing, the TUN seam, and the carry over `comms`). This
mirrors `lait-relay` fronting `comms::relay`: the binary names no transport type
of its own. Together they are the first slice of lait's own **packet layer**: the thing that lets IP packets
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
- **Carries over lait's own transport** (via `comms`): **encrypted** by QUIC,
  reaching peers over lait's relays and NAT traversal (chosen by `--network` /
  `LAIT_NETWORK`), and **re-dialing on its own** when a path drops. One node's
  transport identity *is* its device key — the same key its address derives
  from.
- **Is not, yet**: folded into the running daemon. It is a standalone binary;
  slice 3 makes the carry a `lait/exec/1` plane inside `lait` itself.

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
3. Bring both tunnels up. On a LAN the simplest mode is `isolated` — **no relay,
   no discovery, encrypted direct reach** — where each side is given the other's
   public key and a direct address:
   ```sh
   # On A (B listens on its own machine; iroh picks its UDP port — give A a
   # reachable address for B, e.g. its LAN ip and the port B prints, or use a
   # fixed one your firewall allows):
   sudo lait-net --seed <A-seed-hex> --network isolated \
                 --peer <B-pubkey-hex>@<B-host>:<B-port>
   # On B (the Pi):
   sudo lait-net --seed <B-seed-hex> --network isolated \
                 --peer <A-pubkey-hex>@<A-host>:<A-port>
   ```
   Across a NAT, use `--network public` (peers by public key, discovery + n0
   relays resolve them — no direct address needed) or `--network local --relay
   <url>` to rendezvous through **your own** relay (`lait-relay`) and escape n0
   entirely.
4. From A, ping B's derived address (shown by `--print` on B):
   ```sh
   ping6 <B-address>
   ```
   Packets leave A's TUN, ride an **encrypted** `comms` connection to the Pi,
   enter the Pi's TUN, and the Pi's stack answers — an L3 tunnel addressed
   entirely by device key, over lait's own transport.

`--dev` names the interface (default `lait0`). The lower PeerId dials and the
higher accepts, so one connection forms per pair; a dropped path re-dials.
Unknown-destination packets are dropped.

## The ladder from here to the real packet layer

This crate is slice 1 of the L3 plan (Specs in the Netstack project). In order:

1. **Derived addressing + Linux TUN + packet carry** — *this crate*. ✅
2. **Carry over lait's transport** — the carry rides a `comms` connection
   (iroh under the hood): encrypted, over lait's relays and NAT traversal. ✅
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
