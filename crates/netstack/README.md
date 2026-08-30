# netstack — lait's L3 packet layer

The sealed home of the L3 concern: **addressing** (`src/lib.rs`), the **TUN**
OS seam (`src/tun.rs`), and the **carry** (`src/carry.rs`) that composes
`comms` to move packets. It names `comms` and never iroh — exactly as
`lait-relay` fronts `comms::relay` — so every IPv6 notion stays out of the
transport seam.

This is what lets IP packets between two of a person's devices ride lait's own
fabric instead of a Tailscale tunnel, with **no coordination server** handing
out addresses and **no identity provider** deciding who you are.

## The one idea

**A device's address is derived from its identity.** A lait `DeviceId` *is* an
ed25519 key, and its tunnel address is a hash of that key in the ULA range
`fd00::/8`. No allocator, no authority, no collisions — the address *is* the
key. That is exactly the job Tailscale needs a coordinator for, done with a
hash (`ula_from_key`).

## It is a component, not a program

There is no `netstack` binary. The carry borrows the daemon's identity
transport, follows a `watch` of the profile's own devices, and is mounted by
`src/daemon/netplane.rs` as the daemon's net plane. There was a standalone
`lait-net` front; it was deleted with slice 3, because its only value was a
probe over a code path the product does not run — its own endpoint, a peer
list fixed at boot, and an accept path that admitted anyone who dialed the
ALPN. The probe is now two enrolled daemons and `ping6`.

Admission is one pure function, `carry::admit_own`: QUIC proved the caller's
key, the profile's device set says whether that key is mine. A caller outside
the set is closed before a route exists; a device retired from the kinship log
loses its route while a packet is still in flight.

## Trying it on two Linux boxes (a Raspberry Pi is the case this is for)

Both ends need Linux and `CAP_NET_ADMIN` — the service unit `lait install`
writes carries it, and a desktop daemon started by hand does not (it reports
`interface: not permitted`, which is a fact about the machine and not a
failure). Enrol the second device from the first (Astrolabe → Devices → Add
device), then, with both daemons up:

```sh
# A device's address is `ula_from_key` of its device id, so it never changes
# and either box can compute the other's. The daemon logs its own at mount:
#   interface up  dev=lait0 address=fd…
ping6 <the other device's fd…:: address>
```

Packets leave one TUN, ride an **encrypted** `comms` connection, enter the
other TUN, and that host's stack answers — an L3 tunnel addressed entirely by
device key, over lait's own transport. Retire the device from Astrolabe and
the ping stops within a round trip: the route is the Own relation, and nothing
else.

`LAIT_NET=off` withholds the interface without stopping the daemon.

## The ladder

Slices 1–3 are done. In order:

1. **Derived addressing + Linux TUN + packet carry.** ✅
2. **Carry over lait's transport** — the carry rides a `comms` connection
   (iroh under the hood): encrypted, over lait's relays and NAT traversal. ✅
3. **The net plane** — the carry is mounted in the running daemon under its
   own identity, peers are the profile's own devices, live-subscribed. ✅
4. **Authority → packet filter** — compile the Space's signed per-device
   consent into a per-host filter: which peer may reach which port. This is
   what keeps L3 from meaning "any member reaches everything". The opening on
   every flow carries a `features` word so this is a feature bit rather than
   an ALPN bump.
5. **MagicDNS-equivalent** — resolve address-book petnames to derived
   addresses.
6. **macOS (utun + a Network Extension entitlement) and Windows (WinTun + a
   privileged service)** — the same seam as `src/tun.rs`, behind the gated /
   privileged installs. File the Apple entitlement early; it is the only step
   that waits on someone else. Until then both platforms report
   `Interface::Unsupported`, which is a different fact from "off" and from
   "no peers".
7. **Subnet routers / exit nodes** — full-Tailscale reach, last.
