# Gmail generated client

This directory vendors `google-gmail1` version `7.0.0+20251215`, published by
Sebastian Thiel under the [MIT license](LICENSE.md). The generated Rust sources,
upstream README, and license are unchanged.

- [Published crate](https://crates.io/crates/google-gmail1/7.0.0+20251215)
- [Upstream source at the published revision](https://github.com/Byron/google-apis-rs/tree/961d1c290b3df52be63a5369cfd3c766af6064af/gen/gmail1)
- Crate archive SHA-256: `7c3a243f6e1a552f8fa6f5e92b6032c898c650cb07671b4ac5abf9119d301ab2`
- Original VCS metadata: [.cargo_vcs_info.json](.cargo_vcs_info.json), with its
  final newline normalized.

[wasi.patch](wasi.patch) records the only upstream change: removing
`rustls-native-certs` and `native-tokio` from the normalized manifest's
`hyper-rustls` dependency. The Gmail extension supplies `ring`, `http2`, and
`webpki-tokio` explicitly, retaining HTTPS certificate verification with bundled
WebPKI roots. OAuth tokens continue to come from the Sloper connection.

The package's registry bookkeeping, original unnormalized manifest, and unused
package lockfile are omitted. Dependency resolution uses the repository's root
`Cargo.lock`; this package is excluded from workspace membership and selected by
the root crates.io patch.

The upstream `google-apis-common` dependency has unmaintained-runtime advisory
`RUSTSEC-2025-0066`. The exception and its transport-test coverage are recorded
in the repository's advisory configuration. Updating this vendor requires
reviewing the generated client, reapplying the two-feature patch, updating this
archive checksum, and running Gmail's WASI transport and component tests.
