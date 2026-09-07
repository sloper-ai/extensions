# Sloper public extensions

Build customer follow-ups and mailbox review into your Sloper apps. This collection starts with [Gmail](gmail/README.md): use its message and draft actions in a follow-up workflow, or apply labels as part of a review. You decide how those operations fit your work.

This collection contains the extension sources and the tests for their distributable components. Each extension has its own semantic version and uses an immutable revision of the [extension SDK](https://github.com/sloper-ai/extension-sdk), so you can trace a release back to the code and tools that produced it.

## Build and verify

Install [mise](https://mise.jdx.dev/getting-started.html), then run:

```sh
mise run bootstrap
hk check --all --slow
mise run '//...:build'
mise run '//...:test'
```

Lint and formatter settings live in `pyproject.toml`. Tool versions and tasks are in `mise.toml`, and Git hook definitions are in `hk.pkl`.

Bootstrap also installs Git hooks through mise. Pre-commit formats staged
files; commit messages must follow Conventional Commits. Pre-push runs all
checks, then builds and tests affected projects. Use `hk fix --all` to apply
available fixes. `mise run check` runs the
same full check suite. Vendored sources retain their upstream formatting;
dependency policy checks still cover them.

Mise provides `sloper-extension` from [the pinned SDK revision](extension-sdk.rev). The build produces a stamped component, and the tests exercise those same bytes before publication. See [Gmail's README](gmail/README.md) for the available operations, local test fixtures, and WASI prerequisites.

For redistribution, run `mise run //gmail:license` before `mise run //gmail:build`. This uses pinned `cargo-license` tooling to generate `gmail/ThirdPartyNotices.txt`; the SDK copies that report and Gmail's `LICENSE` alongside the component. Delivery CI generates the report automatically.

## Publish

Releases use the reserved Sloper publisher and a public audience grant, and are available to authenticated Sloper users. CI keeps tested bytes, digests, provenance, and staging receipts before production promotion. Publishing never rebuilds or restamps a component. Configure separate staging and production publishing credentials only in each publish step. See [delivery and promotion](docs/ci.md).

To write a new extension, start with the SDK's [authoring guide](https://github.com/sloper-ai/extension-sdk/blob/main/docs/authoring.md) and examples. Source visibility, release audience, and the code license are separate policies.

## License

Sloper-owned code is source-available under the [Sloper Ecosystem License](LICENSE). Development, redistribution, and business use for Sloper extensions are included without a separate SDK license fee. Reuse outside Sloper requires separate written commercial terms, including noncommercial reuse. See the [usage guide](docs/licensing.md). The vendored Gmail client retains its [MIT license](vendor/google-gmail1/LICENSE.md) and [provenance](vendor/google-gmail1/PROVENANCE.md).
