# Nalanda

A calibre-style ebook manager written in Rust. The desktop app uses
[GPUI](https://www.gpui.rs/) through [gpui-kit](https://gpui-kit.com). The
library also runs as an HTTP/OPDS server, so mobile and web clients can share it.

## Features

- **Library**: import EPUB/KEPUB/PDF/MOBI/AZW3/FB2/TXT/HTML/DOCX/CBZ files, then search and sort them
  (recent, title, author). Files are stored in calibre's on-disk layout, `Author/Title (id)/`.
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
  setting). The reader page has its own Light/Sepia/Dark colors. Preferences are saved to
  `~/.config/nalanda/settings.json` (or the platform's config directory).
- **Content server**: a JSON API and an OPDS feed, either embedded in the desktop app
  ("Start server") or headless via `nalanda-server`.

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

## Building

```sh
cargo test                       # every crate except the desktop app
cargo run -p nalanda-server -- --library ~/Nalanda\ Library
cargo run -p nalanda-desktop       # NALANDA_LIBRARY overrides ~/Nalanda Library
```

On Debian/Ubuntu, the GPUI app needs:

```sh
sudo apt install libasound2-dev libfontconfig-dev libwayland-dev libx11-xcb-dev \
  libxkbcommon-x11-dev libssl-dev libzstd-dev libvulkan1 clang cmake pkg-config
```

The embedded server binds `127.0.0.1:8083`. The API has no authentication yet,
so only set `NALANDA_BIND=0.0.0.0:8083` on a trusted network.

## Not yet implemented

- API authentication; a remote-library mode for the desktop app
- Writing edited metadata back into the book files (only the catalog is updated)
- PDF cover thumbnails (needs a rasterizer such as pdfium); MOBI/AZW3 metadata
- Native conversion beyond text-centric formats; real KEPUB span markup
- Kobo model names, shelves/collections sync, and reading-progress sync back to the library
- Images and inline styling in the reader
