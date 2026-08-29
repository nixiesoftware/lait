# lait's web client

A keyboard-first board over the local control plane. This directory builds into
`../products/issues-app/assets/web`, which is packaged with the independently
shipped Issues World release.

If you are extending lait — a theme, extra commands, a different frontend against the
same control plane — this is the reference. The whole client is a **projection of a
command registry** (`src/core/registry.ts`); read that file first.

## The loop

This directory has no dev server, and that is deliberate — it had one and it was
removed. A Vite server on `:5178` proxying to an engine on `:7717` meant the
client you were looking at was never the client anybody runs: a different
origin, a token carried by a proxy instead of a cookie, and a whole seam that
existed only in development. What it showed you was *a* viewer. It was not the
one in the window.

The client can now serve this World from the tree you are building, so look at
it there:

```bash
(cd viewer && npm run build)          # → ../products/issues-app/assets/web
cargo stage-worlds
```

Then in Astrolabe: **+ Add local World** at the foot of the Library rail, and
pick `target/local-worlds/worlds/com.lait.issues/<version>`. It becomes an entry
of its own — its own id, its own mount, beside the released Issues rather than
replacing it.

After that the loop is short. A head reads a file per request, so a rebuild is
visible on reload:

```bash
(cd viewer && npm run build)
cp -R products/issues-app/assets/web/. \
  target/local-worlds/worlds/com.lait.issues/<version>/
```

Reload the World window. No restart, and nothing to copy by hand.

Changing the **runner** — anything in `products/issues-app/src` or the crates
under it — does need a rebuild, a re-stage, and the World stopped and opened
again from the Library. Stopping and opening is the refresh; there is no
separate reload command, deliberately.

## The production path

`npm run build` writes the bundle straight into
`../products/issues-app/assets/web`, and **that directory is committed to git.**
The generic `lait` host embeds none of it. The desktop bootstrap packager and
the World publication workflow copy the generated tree into the immutable
Issues release beside `world.json` and `lait-world-issues`; subsequent Issues
updates replace that release independently of the host. CI diffs a fresh
rebuild against the committed tree, so stale product bytes fail before publish.

**After editing `src/`, a change is visible two ways:**

| You want | Do |
|---|---|
| To see it in the client | `npm run build`, copy into your local World's tree, reload the window |
| CI to accept the branch | commit the rebuilt `../products/issues-app/assets/web` |

`cargo build` does not consume the web tree. The staging scripts do, which keeps
the host's Rust build product-blind and lets the World ship on its own cadence.

## Layout

| Path | What |
|---|---|
| `src/core/registry.ts` | The command seam. Everything is a projection of this. |
| `src/core/` | Pure, tested logic: keys, filter, overlay, workflow, activity, fuzzy. |
| `src/ui/` | React components. `Picker` is the shared control every field uses. |
| `src/api.ts` | The whole backend: `fetch` over the control plane. |
| `src/types.ts` | Hand-maintained mirror of the engine's Layer-B DTOs. Read the header. |
| `scripts/dev.mjs` | The one-command loop above. |

## Tests

```bash
npm run check        # tsc, no emit
npm test             # vitest — the core/ logic
```

The core is where the tests are, because the core is where the decisions are. A
component test that renders a `Picker` proves less than the `filter`/`overlay`/
`workflow` tests that pin what the client is *allowed to believe*.
