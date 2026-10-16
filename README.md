# Nalanda

A calibre-style ebook manager written in Rust. The desktop app uses
[GPUI](https://www.gpui.rs/) through [gpui-kit](https://gpui-kit.com). The
library also runs as an HTTP/OPDS server, so mobile and web clients can share it.

## Features

- **Library**: import EPUB/KEPUB/PDF/MOBI/AZW3/FB2/TXT/HTML/DOCX/CBZ files, then search and sort them
  (recent, title, author). Files are stored in calibre's on-disk layout, `Author/Title (id)/`.
  The library folder defaults to `~/Nalanda Library`. Use "Change library…" in the sidebar to open
  another library or start a new one in an empty folder. Existing calibre libraries open in place
  with no import step, and edits are written to calibre's own `metadata.db`, so calibre sees them
  too. Before its first change to a calibre library, Nalanda copies `metadata.db` to
  `~/.local/share/nalanda/calibre-backups/` (or the platform's data directory). Don't edit the
  same library in Nalanda and calibre at the same time.
- **Metadata & covers**: EPUB 2/3 metadata (title, authors, series, tags, ISBN, …) and embedded
  covers, plus PDF document info. Metadata can be edited in the app.
- **Reader**: a native reflowable reader for EPUB/TXT/HTML with a table of contents (it also splits
  multi-chapter files at TOC anchors) and adjustable font size.
- **Conversion**: built-in TXT/HTML/EPUB → EPUB/KEPUB/TXT. Other pairs (MOBI, AZW3, PDF, DOCX, …)
  go through calibre's `ebook-convert` when it is installed.
- **Kobo**: detects connected devices and shows their books with reading progress. You can send
  books (converting to EPUB when needed) and delete sideloaded books, which also removes them from
  the device database.
- **Themes**: light, dark, or follow the OS (on Linux through the XDG desktop portal's color-scheme
  setting). The reader page has its own Light/Sepia/Dark colors. Preferences, including the
  library folder, are saved to `~/.config/nalanda/settings.json` (or the platform's config directory).
- **Content server**: a JSON API and an OPDS feed, either embedded in the desktop app
  ("Start server", which can be stopped again) or headless via `nalanda-server`.

## Architecture

```
                 ┌────────────────┐      ┌──────────────────┐
                 │ nalanda-desktop│      │ future mobile app│
                 │     (GPUI)     │      │  / OPDS readers  │
                 └──┬─────────┬───┘      └────────┬─────────┘
          in-process│         │embeds             │ HTTP + JSON / OPDS
                    │   ┌─────▼──────────┐        │
                    │   │ nalanda-server │◄───────┘
                    │   │ (axum)         │
                    │   └─────┬──────────┘
         ┌──────────▼─────────▼──────┐   ┌────────────────┐
         │ nalanda-library (SQLite   │   │ nalanda-device │ ← local USB only
         │  + calibre layout)        │   │ (Kobo)         │
         └──────┬────────────────────┘   └────────────────┘
   ┌────────────▼────┐   ┌─────────────────┐
   │ nalanda-formats │◄──│ nalanda-convert │ → calibre ebook-convert (optional)
   └─────────────────┘   └─────────────────┘
              nalanda-core: shared model = API wire types
```

| Crate             | Role                                                                    |
| ----------------- | ----------------------------------------------------------------------- |
| `nalanda-core`    | `Book`, `Metadata`, `BookFormat`. These double as the API's JSON types. |
| `nalanda-formats` | Metadata/cover extraction, the reader's content model, EPUB writer      |
| `nalanda-library` | SQLite catalog and file storage; `Sync`, shared by desktop and server   |
| `nalanda-convert` | Converter backends (built-in, calibre) and library-level conversion     |
| `nalanda-device`  | Kobo detection, listing, sending and deleting                           |
| `nalanda-server`  | REST + OPDS over a library (`nalanda-server --library DIR --bind ADDR`) |
| `nalanda-desktop` | GPUI app (`nalanda`)                                                    |

Devices are deliberately not part of the server API. A Kobo is plugged into one
machine, so the desktop process talks to it directly.

## Installation (Linux)

Tagged releases publish packages on the
[GitHub Releases page](https://github.com/ninad-bhangui/nalanda/releases).
Prebuilt packages are x86_64. Both packages add Nalanda to your application
launcher and offer it as an application for opening supported ebook files.
You need an X11 or Wayland desktop and a working graphics driver (Vulkan recommended).
Calibre is optional: install it if you want the extra conversion formats provided
by `ebook-convert`. Your library and settings are kept when upgrading or uninstalling.

### Debian / Ubuntu

Download the `.deb` release asset, then install it with APT so its runtime
dependencies are installed too:

```sh
sudo apt install ./nalanda_*.deb
nalanda
```

The prebuilt `.deb` is built on Ubuntu 24.04; older distributions may have
libraries that are too old. On those systems, build from source instead.

### Arch Linux

For a prebuilt release, download the `.pkg.tar.zst` asset and install it:

```sh
sudo pacman -U ./nalanda-git-*.pkg.tar.zst
```

Or build the latest development version using the included `PKGBUILD`:

```sh
sudo pacman -S --needed base-devel git
git clone https://github.com/ninad-bhangui/nalanda.git
cd nalanda/packaging/arch
makepkg -si
```

Run `makepkg` as your normal user, not root. It installs the required build
dependencies, compiles and tests the app, and installs both `nalanda` and
`nalanda-server`. Re-run `makepkg -sif` to rebuild and update to the latest commit.
This is a `nalanda-git` development package, not an AUR submission; no AUR helper
is required. Building can take a while because GPUI has a large dependency graph.

### Headless server

The separate `nalanda-server-linux-x86_64.tar.gz` release asset contains the server
without the desktop application:

```sh
tar -xzf nalanda-server-linux-x86_64.tar.gz
sudo install -m755 nalanda-server /usr/local/bin/nalanda-server
nalanda-server --library ~/Nalanda\ Library
```

Release assets include `SHA256SUMS`. To check downloaded files, put the checksum
file beside them and run `sha256sum --check --ignore-missing SHA256SUMS`.

## Building

```sh
cargo test                       # every crate except the desktop app
cargo run -p nalanda-server -- --library ~/Nalanda\ Library
cargo run -p nalanda-desktop       # NALANDA_LIBRARY overrides the chosen library folder
```

On Debian/Ubuntu, the GPUI app needs:

```sh
sudo apt install build-essential clang cmake pkg-config libfontconfig-dev libfreetype-dev \
  libwayland-dev libx11-xcb-dev libxcb1-dev libxkbcommon-dev \
  libxkbcommon-x11-dev libssl-dev libzstd-dev libvulkan1
```

The embedded server binds `127.0.0.1:8083`. The API has no authentication yet,
so only set `NALANDA_BIND=0.0.0.0:8083` on a trusted network.

### Building a Debian package locally

With a current stable Rust toolchain and the build dependencies above installed:

```sh
cargo install cargo-deb --version 3.6.2 --locked
cargo build --release --locked -p nalanda-desktop
cargo deb -p nalanda-desktop --no-build
```

The package is written to `target/debian/`. Its linked-library dependencies are
detected automatically, with runtime-loaded graphics libraries declared explicitly.

## CI and releases

GitHub Actions runs **CI** on branch pushes and pull requests. It tests every
crate (including desktop), builds Debian and Arch packages, validates the desktop
launcher, and smoke-tests package installation and shared-library availability.
Packages can be downloaded from the workflow run's artifacts before a release.
Arch builds run in an up-to-date Arch container against the exact commit being
tested, including unmerged pull requests.

The **Release** workflow is the CD path. For a release, update
`workspace.package.version` in `Cargo.toml` and the workspace versions in
`Cargo.lock`, commit the changes, then push a matching tag, for example `v0.1.0`.
The workflow rejects a tag that does not match the manifest version, runs the
same CI jobs, and only then publishes a GitHub Release containing:

- Desktop `.deb` and Arch `.pkg.tar.zst` packages
- Standalone server archive
- Source archive and the Arch `PKGBUILD`
- SHA-256 checksums and generated release notes

Publishing uses GitHub's built-in token; no additional release secret is needed.
There is no automatic AUR upload, package repository, or in-app updater yet.
To upgrade, download and install a newer release using the same commands above.

## Not yet implemented

- API authentication; a remote-library mode for the desktop app
- Writing edited metadata back into the book files (only the catalog is updated)
- PDF cover thumbnails (needs a rasterizer such as pdfium); MOBI/AZW3 metadata
- Native conversion beyond text-centric formats; real KEPUB span markup
- Kobo model names, shelves/collections sync, and reading-progress sync back to the library
- Images and inline styling in the reader
