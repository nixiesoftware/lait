# Foundation service images + the browser-native join surface

The Foundation platform runs three lait services as containers, plus one static
surface. This directory holds the **images lait builds**; the
[`nixiesoftware/foundation`](https://github.com/nixiesoftware/foundation) repo
owns **where and how they run** (OpenTofu, one `infra/envs/prod` module, applied
by a human — CI there only `fmt`/`validate`/`plan`s). The split is deliberate:
the wire a service speaks is defined here, next to the code; the deployment is
defined there, next to the rest of the estate.

| Image | Crate | Dockerfile | Served at | Cloud Run service |
|---|---|---|---|---|
| Post + directory + registry | `lait-post` (`crates/post`) | `Dockerfile.post` | `post.foundation.pub` | `foundation-post` |
| Feed doorbell | `lait-feed-notify` (`tools/feed-notify`) | `Dockerfile.notify` | run.app URL (see below) | `foundation-notify` |
| Rendezvous relay | `lait-relay` (`crates/relay`) | `Dockerfile.relay` | `relay.foundation.pub` | edge VM today (see below) |

The **join surface** — the viewer bundle + two wasms a shared
`foundation.pub/i#join=<ticket>` link loads — is static hosting, not a container;
see [The join surface](#the-join-surface-foundationpubi) below.

`config.rs` pins the names the fleet uses: `FOUNDATION_SERVICES`,
`FOUNDATION_RELAY`, `FOUNDATION_NOTIFY`, and `FOUNDATION_JOIN_BASE`
(`https://foundation.pub/i`). Changing any of them is a lait release.

## Building an image

There is no local Docker in the authoring environment, so images are built in
Cloud Build and pinned by digest. One `cloudbuild.yaml` builds any of the three,
parameterized by `_DOCKERFILE` and `_IMAGE`. Build from a lait checkout at the
SHA you intend to ship:

```sh
# Post (the default)
gcloud builds submit --project the-foundation-498604 \
  --config packaging/foundation/cloudbuild.yaml \
  --substitutions=_IMAGE=us-central1-docker.pkg.dev/the-foundation-498604/foundation/lait-post,_TAG=$(git rev-parse --short=8 HEAD)

# Notify
gcloud builds submit --project the-foundation-498604 \
  --config packaging/foundation/cloudbuild.yaml \
  --substitutions=_DOCKERFILE=packaging/foundation/Dockerfile.notify,_IMAGE=us-central1-docker.pkg.dev/the-foundation-498604/foundation/lait-feed-notify,_TAG=$(git rev-parse --short=8 HEAD)

# Relay
gcloud builds submit --project the-foundation-498604 \
  --config packaging/foundation/cloudbuild.yaml \
  --substitutions=_DOCKERFILE=packaging/foundation/Dockerfile.relay,_IMAGE=us-central1-docker.pkg.dev/the-foundation-498604/foundation/lait-relay,_TAG=$(git rev-parse --short=8 HEAD)
```

Read the pushed digest off the build output (or
`gcloud artifacts docker images list … --include-tags`) and hand it to the
Foundation repo as the `image_<name>` value. **The digest is what deploys; the
`latest` tag floats and is not the record.**

> **Build-context traps.** `gcloud` uploads what `.gitignore` admits (there is no
> `.gcloudignore`), and `.dockerignore` strips `packaging/`, `ci/`, `docs/`, and
> most `*.md`. Both Dockerfiles `COPY . .` and `cargo build --locked` under the
> floating `stable` toolchain (`rust-toolchain.toml` overrides the image's pinned
> Rust), so a `Cargo.lock`/MSRV drift only shows up at build time. Reproducible
> digests are not a property here.

## Deploying (in the Foundation repo)

Pinning a digest and rolling a revision is a human `tofu apply` in
`nixiesoftware/foundation` (`infra/envs/prod`); there is no CI apply. In short:
`gh variable set IMAGE_<NAME> …` (the plan input), set the same `TF_VAR_image_*`
locally, `tofu apply`, then verify the service's `/health` (and for Post,
`/directory/health` + `/registry/chronicle`). Rollback is
`gcloud run services update-traffic <service> --to-revisions <prev>=100`. That
repo's `docs/RUNBOOK.md` is the authority on the apply.

### The relay's shape

`lait-relay` behind Cloud Run's TLS termination serves plain HTTP on `$PORT` and
is told its public name — the platform passes
`--http 0.0.0.0:8080 --advertise https://relay.foundation.pub`. This is enough
for the daemon-less browser tab and the consensus-supporter presence fanout,
which ride the relay's HTTP path; browsers cannot holepunch UDP regardless.

**Today `relay.foundation.pub` is a hand-built edge VM, not Cloud Run.** The
Foundation module has a gated-off `foundation-edge` VM (`vm.tf`,
`enable_relay_vm=false`) whose Caddy fronts the relay, and its `image_relay`
variable was a placeholder because no `Dockerfile.relay` existed. This file is
that Dockerfile; whether the relay moves to Cloud Run or the managed VM turns on
is the Foundation repo's call. Either consumes this image.

## The join surface (`foundation.pub/i`)

A shared link is `https://foundation.pub/i#join=<ticket>`. The ticket rides the
URL **fragment**, so it never reaches the server — the page reads it client-side
(`viewer/src/worker/bootstrap.ts`, `parseJoin`), spawns the in-tab wasm engine,
and joins over `FOUNDATION_RELAY` (the default when the link carries no explicit
`&relay=`). So `/i` is **static hosting of three same-origin files**:

- the built viewer bundle — `(cd viewer && npm run build)` → `products/issues-app/assets/web`
- the engine wasm at `/porthole_bg.wasm` — `crates/porthole`, `wasm-pack build --target web` → `pkg/porthole_bg.wasm`
- the World runner at `/lait_issues_runner.wasm` — the release build of the Issues runner

The hosting contract, proven by the e2e static server
(`ci/viewer-e2e/serve.mjs`) and the full run (`ci/browser-viewer-e2e.sh`):

- **Same origin.** Both wasms are fetched from `/porthole_bg.wasm` and
  `/lait_issues_runner.wasm` on the page's own origin. The CDN/bucket must serve
  them there, not from a second host (the CSP the daemon shell ships,
  `src/serve/shell.rs`, is same-origin; a cross-origin wasm fetch is refused).
- **Correct MIME.** `.wasm` → `application/wasm` (streaming compile needs it).
- **SPA fallback.** A request for `/i` (any path without a file extension) serves
  `index.html`; the fragment does the routing.
- **HTTPS.** A secure context is required for the Worker, WebCrypto, and the OPFS
  the tab mints its seed into. `foundation.pub` is HTTPS; nothing else is needed
  — the e2e proves function with no COOP/COEP headers.

The build steps above are the same ones `ci/browser-viewer-e2e.sh` runs; that
script is the reference for assembling the surface. Publishing these bytes to
`foundation.pub/i` (a bucket/CDN behind the apex, or the edge) is a Foundation
deployment step, not yet automated in either repo — the wasms are gitignored
build outputs, so they are produced at publish time, not committed.

> The two wasms are large (~14 MiB engine, ~39 MiB runner) and immutable per
> release — serve them with a long-lived immutable cache, the viewer bundle with
> a short one, exactly as the feed bucket serves release artifacts.
