# Codemods

`astryx upgrade` runs these when a consumer moves across a breaking change in
`@lait/ui`, the same way it runs core's for `@astryxdesign/core`.

Each is a plain object stamped `type: 'code'` (transforms source) or
`type: 'config'` (rewrites the consumer's `astryx.config`), authored against
`AstryxCodemod` from `@astryxdesign/cli/authoring`.

Empty until the first breaking change — which is the point of having somewhere
for it to go before there is one.
