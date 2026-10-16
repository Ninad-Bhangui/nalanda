# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```sh
cargo test --workspace --locked                  # what CI runs (desktop included)
cargo test -p nalanda-formats                    # one crate
cargo test -p nalanda-library some_test_name     # one test by name
cargo run -p nalanda-server -- --library ~/Nalanda\ Library
cargo run -p nalanda-desktop
cargo build --release --locked -p nalanda-desktop -p nalanda-server
```

Use `--locked` so builds never rewrite `Cargo.lock`. No clippy or rustfmt config or CI step exists; run them only if asked.

Desktop builds need the GPUI system packages listed in the README (`libwayland-dev`, `libvulkan1`, etc.). The `default-members` in `Cargo.toml` exclude `nalanda-desktop`, so bare `cargo test` skips it.

## Releases

The `Release` workflow fails unless the pushed tag equals `v` + `workspace.package.version` in `Cargo.toml`. When bumping the version, run `cargo update --workspace` so `Cargo.lock` updates itself. Do not hand-edit `Cargo.lock`. If a tag points at the wrong commit, move the tag to the commit that has the right version.

## Git

Push directly to `main`. Don't create feature branches or PRs unless the user asks. This is a solo, low-risk project.

## Architecture

The big picture spans several crates, so read these together before changing data flow:

- **Shared model.** `nalanda-core` defines `Book`, `Metadata` and `BookFormat`. These types are also the server's JSON wire types, so changing a field changes the API.
- **Library is the hub.** `nalanda-library` owns a calibre library: a `metadata.db` SQLite catalog plus `Author/Title (id)/` folders. Both the desktop app and the server open a library through this crate. Changes here affect both frontends.
- **Format work is separate from the catalog.** `nalanda-formats` extracts metadata and covers, builds the reader's content model and writes EPUB. It doesn't touch the database.
- **Conversion goes through a backend.** `nalanda-convert` picks a built-in converter for text-centric pairs or shells out to calibre's `ebook-convert` for the rest. It's optional at runtime.
- **Devices bypass the server.** `nalanda-device` (Kobo, local USB only) is called directly by the desktop process and is deliberately not exposed through `nalanda-server`.
- **Two frontends, one library.** `nalanda-desktop` (GPUI) calls the library in-process and can start an embedded `nalanda-server` on `127.0.0.1:8083`. `nalanda-server` runs headless and serves a JSON API and OPDS feed. The API has no authentication yet.

Calibre compatibility is a hard constraint. Nalanda must not leave a library that calibre can't open, and it backs up `metadata.db` before its first change to an existing calibre library. Don't edit the same library from Nalanda and calibre at once.

## Status

Not implemented yet (from the README): API authentication, remote-library mode, writing metadata back into book files, native conversion beyond text formats, and reading-progress sync to the library.
