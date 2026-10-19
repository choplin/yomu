# Release Automation

Yomu uses one release tag and two tools with non-overlapping ownership.

| Side effect | Owner |
| --- | --- |
| Version and changelog update | `cargo-release` |
| Release commit | `cargo-release` |
| First crates.io publication | Local `cargo-release` |
| Later crates.io publication | `Publish crate` workflow through Trusted Publishing |
| `v<version>` tag and push | `cargo-release` |
| Binary builds and archives | `dist` |
| SHA-256 checksums | `dist` |
| GitHub Release and artifact upload | `dist` |
| Homebrew formula generation and tap publication | `dist` |

Run `cargo-release` locally. It owns the version and changelog update, release
commit, `v<version>` tag, and push. For the first version it also publishes the
crate with the credentials installed by `cargo login`. For later versions,
pass `--no-publish` so the tag workflow owns crates.io publication.

After the first version exists on crates.io, configure a Trusted Publisher for
GitHub owner `choplin`, repository `yomu`, and workflow file
`publish-crate.yml`, with no environment restriction. For later versions, the
tag-triggered `Publish crate` workflow exchanges GitHub's OIDC identity for a
short-lived crates.io token. It stores no long-lived crates.io or repository
token. The workflow verifies that the tag matches `Cargo.toml`, and skips
publication when that exact version already exists; this makes the first,
locally published tag safe to push through the same workflow.

The tag also triggers the generated `Release` workflow, where `dist` builds and
publishes the binary artifacts and updates `choplin/homebrew-tap`. The dist
workflow does not publish to crates.io, update versions, or create commits or
tags in the Yomu repository. It creates and pushes the formula commit in the
Homebrew tap using a `HOMEBREW_TAP_TOKEN` with contents write access to that
tap. `RELEASE_GITHUB_TOKEN` and `CARGO_REGISTRY_TOKEN` repository secrets are
not used.

The `Release` workflow also creates the GitHub Release. Its release-notes
section is the content beneath the matching `## [<version>]` heading in
`CHANGELOG.md`; dist then appends the generated Homebrew installation command,
artifact download table, and checksums. `create-release = true` is explicit in
`dist-workspace.toml`, and the generated workflow must remain the only GitHub
Release creator.

The generated `.github/workflows/release.yml` is derived from
`dist-workspace.toml` by `dist` 0.33.0. Do not edit the workflow by hand;
change the dist configuration and regenerate it. The dist configuration pins
every referenced GitHub Action to an audited commit. cargo-binstall reads the
metadata in `Cargo.toml` and installs the `{name}-{target}.tar.xz` archive
produced by `dist`, so it introduces no independent release side effect.

The Nix flake exposes the source build as `yomu` and the checksummed archives
published by `dist` as `yomu-bin`; it intentionally defines no default
package. After publishing a release, update the flake's release version and
artifact hashes on the default branch. This separate update avoids requiring
the release tag to refer to artifacts that do not exist until its workflow has
completed.

## Installation paths

After a release, users can choose one of these equivalent distribution paths:

```console
cargo install yomu --locked
brew install choplin/tap/yomu
cargo binstall yomu
nix profile install github:choplin/yomu#yomu-bin
nix profile install github:choplin/yomu#yomu
cargo install --git https://github.com/choplin/yomu --locked
cargo install --path . --locked
```

The prebuilt Nix package and first three commands consume published release
outputs. The source Nix package, Git, and local-checkout commands build source
directly and are useful before or between releases.

## Validate a release

Install `cargo-release` 1.1.6 and `dist` 0.33.0, then run the package and both
planning paths from a clean checkout with Rust 1.92.0 or newer:

```sh
cargo package --locked
VERSION=x.y.z
cargo release --dry-run "$VERSION"
dist plan
dist generate --check
```

These commands do not publish, commit, tag, push, create a GitHub Release, or
update the Homebrew tap. The cargo-release dry run validates the single
`[Unreleased]` replacement in `CHANGELOG.md`, repeats package verification,
and prints the planned release actions. The dist plan must contain only these
targets and include the Homebrew installer and publisher:

- `aarch64-apple-darwin`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `x86_64-unknown-linux-gnu`

Pull requests run the generated dist workflow in `upload` mode. They build the
same target matrix and retain archives and SHA-256 checksums as workflow
artifacts without creating a GitHub Release or modifying the tap.

The automated commands do not prove terminal presentation. Before publishing,
repeat the Text, Kitty, and experimental sixel procedures in the README with
`examples/showcase.md` and `examples/showcase-ja.md`. Record which terminals
were actually observed; do not turn an unrun matrix entry into a support claim.

## Publish the first release

1. Confirm `main` is clean and all required checks pass.
2. Confirm `cargo login` has installed a valid crates.io token locally and the
   `HOMEBREW_TAP_TOKEN` Actions secret can write to `choplin/homebrew-tap`.
3. Publish the initial crate and create and push its release commit and tag:

   ```sh
   cargo release --execute --no-confirm 0.1.0
   ```

4. Confirm crates.io contains `yomu 0.1.0`.
5. Confirm the `Publish crate` workflow skips the already-published version,
   and the `Release` workflow publishes the four archives, SHA-256 checksums,
   GitHub Release, and Homebrew formula.
6. In the `yomu` crate settings on crates.io, add the Trusted Publisher
   `choplin/yomu` with workflow filename `publish-crate.yml` and no environment.

## Publish later releases

1. Confirm `main` is clean, all required checks pass, the Trusted Publisher is
   still configured, and `HOMEBREW_TAP_TOKEN` is available.
2. Create and push the release commit and tag locally:

   ```sh
   VERSION=x.y.z
   cargo release --execute --no-confirm --no-publish "$VERSION"
   ```

3. Confirm the `Publish crate` workflow publishes through OIDC, and the
   `Release` workflow publishes the expected binary and Homebrew outputs.
4. Update `releaseVersion` and the corresponding artifact hashes in `flake.nix`.
5. Install through Homebrew, cargo-binstall, and Nix, then verify
   `yomu --version`.

If the initial cargo-release publication succeeds but its release commit or tag
push fails, do not publish the crate again. Inspect the local commit, local tag,
remote branch, and remote tag, then resume only the missing step for the same
version. For later releases, a failed Trusted Publishing job can be rerun
against the existing tag; its exact-version check makes reruns safe after a
successful upload. The dist workflow can likewise be rerun after the tag exists
because it only consumes that tag.
