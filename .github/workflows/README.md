# Workflow Strategy

The workflows in this directory are split so that pull requests get fast, review-friendly signal while `main` still gets the full cross-platform verification pass.

## Pull Requests

- Required checks run against GitHub's synthetic merge commit, not the pull
  request head alone. This includes changes already on `main` and catches
  conflicts before they reach the branch.
- `bazel.yml` is the main pre-merge verification path for Rust code.
  It runs Bazel `test` and Bazel `clippy` on the supported Bazel targets,
  including the generated Rust test binaries needed to lint inline `#[cfg(test)]`
  code.
- `rust-ci.yml` keeps the Cargo-native PR checks intentionally small:
  - `cargo fmt --check`
  - `cargo shear`
  - `argument-comment-lint` on Linux, macOS, and Windows
  - `tools/argument-comment-lint` package tests when the lint or its workflow wiring changes

## Post-Merge On `main`

- `bazel.yml` also runs on pushes to `main`.
  This re-verifies the merged Bazel path and helps keep the BuildBuddy caches warm.
- `rust-ci-full.yml` is the full Cargo-native verification workflow.
  It keeps the heavier checks off the PR path while still validating them after merge:
  - the full Cargo `clippy` matrix
  - the full Cargo `nextest` matrix via per-platform archive-backed shards
  - Windows ARM64 nextest archives cross-compiled on Windows x64, then replayed on native Windows ARM64 shards
  - release-profile Cargo builds
  - cross-platform `argument-comment-lint`
  - Linux remote-env tests

## Selected Manual Platform Tests

`rust-ci-full.yml` accepts optional `cargo_package` and `test_filter` dispatch
inputs. Setting either runs selected tests on the `selected_platform` runner:
`linux` (the default, Ubuntu 24.04 x64), `windows` (Windows 2025 x64), or
`macos` (macOS 15 ARM64). These are standard GitHub-hosted runners and reuse
through the existing archive-backed nextest workflow. The inputs are passed as
quoted arguments; they are not shell commands. This mode needs no private
runner group or BuildBuddy secret and retains runtime test-helper setup.

For clipboard validation, select package `codex-tui` and nextest expression
`test(clipboard_copy)`. The archive still compiles that package's complete test
binaries, including worker integration tests. Inspect the shard reports and test
counts to confirm the intended tests ran. Nextest 0.9.111 is explicitly pinned.
Archive-backed shards upload JUnit from the remapped workspace store at
`codex-rs/target/nextest/default/junit.xml` and fail when the report is missing;
this store is separate from the extracted binary target directory.

Selected runs do not emit `Full CI results` and do not satisfy the normal full
cross-platform gates. Leave package and filter empty for the unchanged full
suite; `selected_platform` is ignored in that mode. Existing normal runner
groups, remote environments, and platform requirements continue to apply.

## Rule Of Thumb

- If a build/test/clippy check can be expressed in Bazel, prefer putting the PR-time version in `bazel.yml`.
- Keep `rust-ci.yml` fast enough that it usually does not dominate PR latency.
- Reserve `rust-ci-full.yml` for heavyweight Cargo-native coverage that Bazel does not replace yet.
