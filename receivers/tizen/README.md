# Astrolabe receiver for Samsung Tizen TV

This packaged TV web application is a production Astrolabe Display receiver.
It enrolls through the two-sided confirmation ceremony, stores receiver state
in Tizen KeyManager, authenticates every assigned-program and asset request,
stages bounded verified frames, consumes eligible assignment-bound CMAF streams
through MSE, and continues an eligible frame program from monotonic time while
delivery is offline.

It targets Tizen 5.5 or newer and communicates only with
`https://nixiesoftware.com`. The manifest intentionally does **not** request the
legacy `http://tizen.org/privilege/keymanager` privilege; Samsung prohibits
declaring it on Tizen 3.0 and later.

## Verify and package

1. Install Tizen Studio, the TV extensions, and Samsung Certificate Extension.
2. Create an author/distributor certificate profile for Nixie Solutions LLC.
3. Run `npm test` in this directory.
4. Run `tizen build-web -- receivers/tizen/app` from the repository root.
5. Run `tizen package -t wgt -s <certificate-profile> -- receivers/tizen/app/.buildResult`.
6. Install the signed WGT on a Tizen 5.5+ TV or emulator and complete the
   physical-device qualification checklist in `../QUALIFICATION.md`.

The package ID and application ID in `app/config.xml` are reserved source
identifiers. Confirm them against the Seller Office application record before
the first submitted build; changing them after publication creates a different
application lineage.
