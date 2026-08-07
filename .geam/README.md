# Geam Gleam packaging

The `geam-release` branch follows immutable Gleam release tags and carries the
Cargo metadata, generated upstream-version adaptations, and automation needed
to publish `gleam-core` for Geam.

`release.toml` is the canonical release record. The packaging tool derives all
five package manifests from it. Compiler Rust source remains unchanged except
for the compiler version and Hex user-agent declarations, which stay at the
upstream release version instead of exposing the downstream Cargo package
revision.

The five packages are published under Geam-specific Cargo package names while
preserving their upstream Rust crate names:

```text
geam-gleam-erlang-term-format
-> geam-gleam-erlang-generation

geam-gleam-pretty-arena ----\
geam-gleam-hexpm ------------+-> geam-gleam-core
geam-gleam-erlang-generation /
```

All mirror-to-mirror dependencies use the exact packaging version. Dependency
pins in `release.toml` record release-specific constraints that an upstream
workspace lockfile would otherwise hide from a registry consumer.

```text
upstream release tag
-> geam-sync-upstream workflow
-> sync pull request
-> geam-release
-> geam-verify-packaging workflow
-> draft GitHub release
-> approved geam-publish-crates workflow
-> crates.io and GitHub prerelease
```

The draft release is the final reversible boundary before publication. The
manual publish workflow runs in the protected `crates-io` environment and uses
crates.io Trusted Publishing to obtain a short-lived token. No registry token
is stored in GitHub.

Packages are published in dependency order. Cargo can only generate the final
registry-shaped lockfile for a dependent package after its mirrored dependency
is visible in the crates.io index, so the workflow compares regenerated
archives with the reviewed draft and permits only `Cargo.lock` to differ. It
then replaces the draft asset with the exact archive sent to crates.io. A retry
skips an existing version only when its registry archive is byte-for-byte
identical.

The publish workflow always checks out the immutable commit recorded by the
prepared release rather than repackaging a newer branch commit under the same
version. If the publishing workflows change after a draft is prepared but
before publication, wait for packaging verification to refresh the draft;
stale workflow definitions are rejected before any crate is uploaded. Once any
package in a version is visible on crates.io, draft preparation leaves that
release untouched.

## Workflows

| Workflow | Trigger | Responsibility |
| --- | --- | --- |
| `Geam: Sync upstream release` | Daily schedule or manual dispatch | Select a stable upstream release tag, merge it into a sync branch, regenerate the overlay, verify packages, and open a PR to `geam-release`. |
| `Geam: Verify packaging` | PR, `geam-release` push, weekly schedule, or manual dispatch | Test the overlay and mirrored crates, build reviewed `.crate` candidates, and run current Geam tests and Clippy against those archives. |
| `Geam: Prepare draft release` | Successful packaging verification on `geam-release` | Attach the verified archives and checksums to a draft GitHub Release. |
| `Geam: Publish compiler crates` | Manual dispatch from `geam-release` | Reconcile the exact release archives, publish in dependency order through Trusted Publishing, and publish the GitHub prerelease. |

The fork must enable GitHub's **Allow GitHub Actions to create and approve pull
requests** repository option for the sync workflow to open its PR. The workflow
requests pull-request write permission only to create the PR; it does not
approve or merge it. Default workflow permissions remain read-only.

Upstream release, container, and nightly-release workflows are removed by the
overlay generator. Upstream CI remains available for source compatibility.

## Trusted Publishing

The repository has a protected GitHub environment named `crates-io`. Each of
the five published packages configures the same crates.io Trusted Publisher:

```text
owner: panarch
repository: gleam
workflow: geam-publish-crates.yml
environment: crates-io
```

The configuration is registered separately for each package because
authorization belongs to the crates.io package, while one workflow publishes
the complete dependency graph. Dispatch requires the exact prepared release
tag and is accepted only from `geam-release`.

## Updating a release

The scheduled workflow selects GitHub's latest stable Gleam release. A specific
tag or a dry run can be selected from workflow dispatch. Locally, the equivalent
overlay command is:

```sh
cargo run --manifest-path .geam/tool/Cargo.toml -- \
  set-release \
  --tag v1.18.1 \
  --commit 4a83802ca33a8a96227a1b332768725f232f9779 \
  --revision 1
cargo metadata --format-version 1 > /dev/null
```

`set-release` regenerates manifests and README files from the recorded tag,
preserves the upstream compiler version, and removes upstream publishing
automation. A changed upstream compiler-version declaration fails generation
instead of silently producing a broader patch.

## Local verification

```sh
cargo run --manifest-path .geam/tool/Cargo.toml -- apply
cargo test --manifest-path .geam/tool/Cargo.toml --locked
cargo run --manifest-path .geam/tool/Cargo.toml -- verify
cargo run --manifest-path .geam/tool/Cargo.toml -- package
cargo run --manifest-path .geam/tool/Cargo.toml -- verify-consumer --geam ../geam
```

`verify-consumer` extracts the reviewed `.crate` candidates, rewrites a clean Geam
checkout to use registry-shaped exact dependencies, and supplies only those
archives through local Cargo patches. It then runs Geam tests and Clippy and
records both repository commits and every package SHA-256 in
`.geam/target/package/geam-verification.json`.

Do not use GitHub's generic **Sync fork** action for this branch. Upstream
changes enter through reviewed release-tag sync pull requests.
