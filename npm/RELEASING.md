# npm development and release

The next coordinated version is **0.3.0**. Keep `Cargo.toml`, `Cargo.lock`, the
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
   publishing token is needed for that configured workflow.

The publish script verifies already-published versions by tarball integrity and
skips only identical contents. It checks registry visibility before advancing
and refuses to overwrite a different package. Prerelease versions use the
`next` dist-tag; ordinary versions use `latest`. For a partial failure, rerun the
failed publishing job with its original build artifacts. GitHub asset retries
likewise verify existing bytes before skipping them. Rebuilding an already
published version can change artifacts; bump the version instead of replacing it.

See [npm trusted publishing](https://docs.npmjs.com/trusted-publishers/) for
account-side configuration and current CLI requirements.
