# Currency module

Official rayslash module for explicit three-letter currency conversions using Frankfurter's free public latest-rates API at `api.frankfurter.dev`. Rates are cached through the bounded host cache so a previously successful pair remains available during a service outage.

Build the release component with `./scripts/build-release.sh`. The script formats regenerated bindings with the pinned Rust 1.92.0 toolchain and verifies that generation leaves `src/bindings.rs` unchanged.
