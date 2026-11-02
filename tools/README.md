# Development tools

Run these from the repository root. Keep test data outside `~/.local/share/booklub`.

## End-to-end run

```bash
cargo build -p booklub-app --features dev-tools
mkdir -p /tmp/bk/data /tmp/bk/cache /tmp/bk/fixtures
cargo run -q -p booklub-core --example make_fixtures -- /tmp/bk/fixtures
XDG_DATA_HOME=/tmp/bk/data XDG_CACHE_HOME=/tmp/bk/cache BOOKLUB_E2E=1 BOOKLUB_E2E_XLSX=/tmp/bk/fixtures/reading-list.xlsx BOOKLUB_E2E_V1=/tmp/bk/fixtures/web-export.json target/debug/booklub
```

Build with `--features dev-tools` right before these runs. `cargo test` overwrites `target/debug/booklub` with a build that has no hooks, and that build just opens and waits. Use a fresh data folder each run. The run replaces your clipboard, and its paste checks fail if another window takes focus.

## Screens and demo data

```bash
cargo run -q --release -p booklub-core --example seed_demo -- /tmp/bk/demo/data /tmp/bk/demo/cache 96
tools/shot.sh /tmp/bk/demo/data /tmp/bk/demo/cache dark home /tmp/bk/home.png
```

## Performance

```bash
XDG_DATA_HOME=/tmp/bk/demo/data XDG_CACHE_HOME=/tmp/bk/demo/cache BOOKLUB_BENCH=1 target/debug/booklub
```

Build with `--release --features dev-tools` for numbers worth quoting.

## Package

`cargo-deb` is installed under `target/tools/bin`. If `target/` was cleaned, reinstall it with `cargo install cargo-deb --locked --root target/tools`.

```bash
PATH="$PWD/target/tools/bin:$PATH" tools/build_release.sh
```

Publish only packages built this way. A plain `cargo deb` skips the checks below, and it stops with a missing file if the notices have not been generated.

The script runs `tools/third_party_notices.py`, which needs `python3` and writes `target/release/THIRD-PARTY-NOTICES.txt`. The package installs it to `/usr/share/doc/booklub/`. It holds Booklub's licence, the licence files of every Rust crate compiled into the app, and the copyright file of the Rust standard library, because their licences require those notices to travel with the binary. Identical texts are grouped.

The build stops if a crate has no licence file, or if its files don't contain the licence it declares. If it names a licence the script doesn't know yet, add that licence to `LICENCE_MARKERS`. If a crate really ships no text, replace the crate or get the text from its authors rather than removing the check.

## Publishing a release

Build the package, then attach it under the stable name that the README's download link uses. Replace `X.Y.Z` with the version in `Cargo.toml`.

```bash
cp dist/booklub_X.Y.Z-1_amd64.deb dist/booklub_amd64.deb
gh release create vX.Y.Z dist/booklub_amd64.deb --title "Booklub X.Y.Z" --generate-notes
```
