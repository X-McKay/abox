# CI-Safe Test Scripts

Scripts in this directory are safe to run in any environment, including
GitHub Actions runners. They require no KVM, no VM bootstrap, and no
host credentials.

Currently, CI-safe checks (`just check`, `just deny`) are pure
cargo/just commands with no wrapper scripts. This directory exists to
establish the `ci/` vs `local/` convention so future CI-only test
scripts have a home.

See `scripts/local/` for tests that require KVM, VM artifacts, or
real API credentials.
