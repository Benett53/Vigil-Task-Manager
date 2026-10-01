# Contributing to Vigil

Thanks for helping! Vigil aims to be a stable, efficient and friendly task manager, Linux first.

## Development setup

```sh
cargo run                       # debug build
cargo run -- --page performance # open a specific page
cargo run -- --snapshot         # test the collectors without a display
```

Before opening a pull request, please run:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
# Windows code is compiled in CI; locally you can check it with:
rustup target add x86_64-pc-windows-gnu
cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings
```

## Guidelines

- **Stability first.** Collectors must never panic on unexpected input. Treat every file in `/proc` and `/sys`
  as potentially missing, unreadable, truncated or changing between reads (processes exit at any time).
- **Efficiency.** Code in the sampling path runs every tick for hundreds of processes. Avoid allocations,
  avoid spawning programs and cache everything that cannot change during a process' lifetime.
- **No blocking in the UI thread.** Anything that may take time (spawning `systemctl`, polkit prompts, file system scans)
  runs as a background job (`VigilApp::spawn_job`).
- **Pure parsers.** Put parsing into pure functions that take `&str` and add unit tests with real-world samples.
- **Portability.** Linux code must work without systemd, without NVIDIA software and on both X11 and Wayland.
- **English** for all user-facing text, code and comments.

## Project layout

See the "How it works" section in the [README](README.md).

## Releasing

Push a tag `vX.Y.Z`; the `Release` workflow builds the Linux (glibc 2.28, x86_64 + aarch64: tar.gz, .deb, .rpm)
and Windows (zip) artifacts and publishes them with SHA-256 checksums.
