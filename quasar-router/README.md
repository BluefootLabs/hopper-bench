# quasar-router

Blueshift Quasar implementation of the router parity contract
(`../ROUTER_CONTRACT.md`, v1) for the cross-framework router benchmark.
Fixed program id `[0x51; 32]`
(`6URwbPipuA4MJLG7LCRRZuWnms3JZ9cRG3z9indXWz8G`), recorded in the
contract's id table.

## Build isolation

This crate is **not** a member of the hopper-bench workspace: Quasar's
dependency graph (zeropod 0.3, solana-account-view 2.0,
solana-instruction-view =2.0, ...) would collide with the workspace
lock (mollusk-svm 0.10.3 pin). It carries its own empty `[workspace]`
table and its own `Cargo.lock`, and path-depends on the local read-only
Quasar checkout:

- Quasar checkout: `E:/Frameworks/quasar` (read-only, do not modify)
- Quasar commit: `37e8a6b`

If your Quasar checkout lives elsewhere, update the `quasar-lang` path
dependency in `Cargo.toml`.

## Build

From this directory:

```
cargo build-sbf
```

Toolchain used for the recorded artifacts: `cargo-build-sbf 4.0.0`,
`platform-tools v1.53`. (Quasar's own Makefile pins `--tools-version
v1.52`; the local v1.53 toolchain builds this crate and Quasar's
examples successfully.)

Then copy the artifact where the runner looks for it:

```
copy target\deploy\quasar_router.so ..\target\deploy\quasar_router.so
```

Host tests (wire codec + error mapping):

```
cargo test
```

## Run the benchmark

From the hopper-bench workspace root:

```
cargo run -p router-bench --release -- \
    --hopper-root <hopper checkout> \
    --out-dir results/<run name>
```
