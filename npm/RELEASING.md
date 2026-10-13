# npm development and release

The next coordinated version is **0.3.2**. Keep `Cargo.toml`, `Cargo.lock`, the
SDK manifest and both platform manifests (including optional dependency pins)
at the same version. Commit all runtime changes and test fixtures required by
the full suite before tagging. The repository may contain unrelated local work;
review the final commit contents explicitly.

## Local build and verification

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
npm ci --prefix npm --ignore-scripts
npm run build --prefix npm
# Use darwin-arm64 on Apple Silicon. This packages the supplied executable.
node npm/scripts/stage.mjs linux-x64 target/release/vhs-rs
npm test --prefix npm
```

For a distributable Linux release, supply the **musl** executable:
`target/x86_64-unknown-linux-musl/release/vhs-rs`. The CI workflow builds this
with `musl-tools`. A locally staged GNU build is useful for development but is
not the portable Linux release artifact. Stage checks the executable version;
CI controls and tests the actual target used for publication.

`stage.mjs` writes npm tarballs and the original installer-compatible GitHub
archives into `npm/artifacts/`. It includes the embedded-font license notices.
`test:package` serves the actual tarballs from a temporary localhost registry,
installs a clean consumer with scripts disabled, shuts that registry down,
compiles and runs TypeScript, and runs the SDK/protocol/lifecycle suite offline.
Compiler/global-binary guards fail the test if installation or execution calls
Cargo, rustc, or a global vhs-rs. No test packages are published.

`test:release` tests publication ordering, retries and integrity failures using
a fake npm executable. It never contacts npm. The default `npm test` runs both.

## First publication (one-time account setup)

The three package names are `@cbxss/vhs-rs-linux-x64`,
`@cbxss/vhs-rs-darwin-arm64`, and `@cbxss/vhs-rs`. The publishing account must
have access to the `@cbxss` scope. This checkout does not contain credentials.

1. Run CI on the reviewed commit and require both Linux and macOS verification
   jobs to pass. Each runs the full Rust suite and installed package tests on
   Node 22 and 24. Download the `release-linux-x64` and `release-darwin-arm64`
   artifacts into separate directories under `release-artifacts/`.
2. Inspect the exact packages without publishing:
   `node npm/scripts/publish.mjs release-artifacts`.
   It requires both binaries, matching versions, and identical SDK tarballs
   from both builds.
3. In an authenticated terminal, run `npm login` and `npm whoami`. Publish the
   two platform tarballs first, then the SDK tarball, using
   `npm publish <tarball> --access public`. Complete npm's authentication/2FA
   steps as prompted. For accounts already configured for unattended publishing,
   `node npm/scripts/publish.mjs release-artifacts --publish` performs the ordered
   publish with integrity checks. The script itself does not handle interactive
   authentication prompts.
4. Configure a trusted publisher in npm for **each** package: GitHub owner
   `cbxss`, repository `vhs-rs`, workflow filename `release.yml`. Allow direct
   `npm publish`. This setup follows the first authenticated publication because
   it is configured on the package settings page.
5. Future `v<version>` tags run the verification workflow before publishing the
   platform packages, then the SDK, then the GitHub installer archives. Release
   jobs use Node 24, npm 11.16.0 and `id-token: write` for OIDC. No stored npm
   publishing token is needed for that configured workflow. Alternatively, set
   the encrypted `NPM_PUBLISH` repository secret to a token with publishing
   access to all three packages. The publishing step supplies it through a
   temporary npm config and removes that config when the step exits.

The publish script verifies already-published versions by tarball integrity and
skips only identical contents. It checks registry visibility before advancing
and refuses to overwrite a different package. Prerelease versions use the
`next` dist-tag; ordinary versions use `latest`. For a partial failure, rerun the
failed publishing job with its original build artifacts. GitHub asset retries
likewise verify existing bytes before skipping them. Rebuilding an already
published version can change artifacts; bump the version instead of replacing it.

GitHub only allows reruns for 30 days. If the original run is older, dispatch
`release.yml` from the current branch with its original Release run ID:

```sh
gh workflow run release.yml -f source-run-id=34643339627
```

Recovery requires a completed tag-push Release run, successful Linux and macOS
verification, and a tag that still points at the original commit. It downloads
that run's retained artifacts and checks out that commit for publication; it
does not rebuild. Existing npm packages and GitHub assets must match exactly.
Expired artifacts cannot be recovered this way.

An npm `E404` during publication can mean the OIDC exchange was rejected, even
when the package exists. Publishing logs include npm's verbose OIDC diagnostics.
Check the trusted publisher settings for all three packages: `cbxss`, `vhs-rs`,
`release.yml`, no environment name, and direct `npm publish` allowed. Newly
configured publishers expire if their first successful publish does not happen
within two days; recreate an expired configuration. Without `NPM_PUBLISH`, the
workflow uses OIDC without a token-auth `.npmrc`. With the secret present, npm
can fall back to token authentication if the OIDC exchange is rejected.

See [npm trusted publishing](https://docs.npmjs.com/trusted-publishers/) for
account-side configuration and current CLI requirements.

## Rust crate publication

The Linux verification job also packages the Rust crate from its clean checkout
and retains `crate-receipt.json` with the version, source commit, Rust toolchain,
and SHA-256. Publication requires that receipt and the original `.crate` file.
The publisher checks out the same commit and installs the recorded toolchain;
Cargo's repackaged archive must match before it can be uploaded. Published
checksums must match before a retry is skipped. Registry errors other than an
exact-version 404 stop publication.

The release workflow uses the encrypted `CARGO_REGISTRY_TOKEN` secret when
present, matching the Eoka publishing setup. Without it, configure a crates.io
trusted publisher for GitHub owner `cbxss`, repository `vhs-rs`, workflow
`release.yml`, and no environment name. The pinned official authentication
action obtains and later revokes a temporary token. Tokens are never stored in
source files or printed. npm uses its package-specific trusted publishers or
the encrypted `NPM_PUBLISH` secret described above.

The workflow validates crate identity and selects authentication before npm
publication. Upload order is platform npm packages, SDK, Rust crate, then GitHub
assets. Legacy source commits without the crate publisher retain their original
npm/GitHub-only recovery path. Modern releases missing a crate or receipt fail.

After publication, Linux and macOS jobs freshly install through npm, the pinned
GitHub installer, and `cargo install --locked --version <version> vhs-rs`. They
compare npm/installer binary hashes to the original artifact, run the full SDK
suite against the registry installation, and execute the public examples using
the installer and Cargo-built binaries. A release is complete only when both
installation jobs pass. Recover partial publication with its original artifacts;
do not rebuild and overwrite a version.

For a new release, update Cargo and all npm manifests and exact binary pins,
run the full CI matrix, and tag the reviewed merged commit. Test fixtures derive
current versions from manifests; historical recovery fixtures stay historical.
The earlier 0.3.1 recovery skipped npm uploads and did not prove fresh OIDC
publication. Check registry bindings or tokens before tagging.
