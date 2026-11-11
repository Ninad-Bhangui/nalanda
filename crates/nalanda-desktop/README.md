# nalanda-desktop

The Nalanda desktop app uses [Rune](https://github.com/anomalyco/rune), pinned to a `dev` rev
in the workspace `Cargo.toml`. It uses the same library, formats, convert, device and server
crates, and shares `settings.rs` and `server.rs` with the GPUI comparison app. It has the same screens: the library grid,
details, the metadata editor, devices, the reader, the content server and themes.

```sh
cargo run --release -p nalanda-desktop
NALANDA_LIBRARY=lib target/release/nalanda --snap out.png --scene selected   # headless
```

Scenes for `--snap`/`--tree`: `selected`, `editing`, `reading`, `light`, `dark`.

## Differences from the GPUI app

- **Add books** opens one file (with an ebook filter) or, with **Add folder…**, one folder.
  Rune's portal dialog picks a single path, while GPUI's picker can select several at once.
  Drag-and-drop still takes many files, on X11. winit doesn't report file drops on Wayland.
- **Confirmations** are an in-window rune-kit `Dialog`, not a native prompt.
- **The reader** builds only the paragraphs near the viewport (`virtual_rows`), where GPUI
  laid out the whole chapter.
- **Covers** are decoded on three worker threads into `rune::Pixels`, in an LRU cache capped
  at 96 MB of decoded RGBA. The view never touches the disk.
- **Headless runs** never write the user's settings file.

## Benchmark against `nalanda-gpui`

Both apps ran from `--release` builds on a 2,000-book library made by
`cargo run --release -p nalanda-desktop --example make_library -- DIR 2000`. Every book has a
1200×1800 JPEG cover. Each app ran alone in a headless sway with one 2560×1640 output at
scale 2, so the window was 1280×820 points.

The driver was `bench/bench.py`: 7 rounds, alternating which app starts first, reporting
medians. Scrolling used a `zwlr_virtual_pointer_v1` wheel, with 150 notches for GPUI and 277
for Rune, so both covered the same distance. Rune scrolls fewer points per
notch. End-of-run screenshots confirmed both stopped on the same rows. Test machine:
4-core x86_64, GTX 1070 Ti (Vulkan), Linux 6.12, Rust 1.99.

| Median of 7 | GPUI (`nalanda-gpui`) | Rune (`nalanda`) |
| --- | --- | --- |
| Launch to first frame mapped | 277 ms | **193 ms** (−30%) |
| RSS, 4 s after launch | **244 MB** | 254 MB |
| CPU over 10 s idle | 30 ms | **10 ms** |
| CPU while scrolling the same distance (5.5 s window) | 6.18 s | **5.65 s** (−9%) |
| RSS after the scroll | **315 MB** | 335 MB |
| Release binary | 65 MB | 59 MB |

Notes:

- **Scroll CPU is mostly cover decoding.** Both apps decode a full 1200×1800 JPEG for each
  thumbnail they reach, so this column compares the whole app, not just the UI framework.
  Rune was lower in all 7 rounds: 5.55–6.02 s against 6.13–6.29 s.
- **Launch** is the time from spawn until sway maps the window. A Wayland window maps on its
  first committed buffer. Rune was faster in all 7 rounds (178–231 ms against 261–295 ms).
- **Rune's memory** is about 10–20 MB higher. Its cover cache allows up to 96 MB and keeps
  every thumbnail scrolled past. GPUI's asset cache holds them too. I haven't profiled where the difference comes from.
- **Not measured:** GPU time, frame pacing and dropped frames. Sway reports neither for a
  headless output.

Reproduce:

```sh
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WAYLAND_DISPLAY= sway -c crates/nalanda-desktop/bench/sway.conf &
source crates/nalanda-desktop/bench/nested.sh
NALANDA_LIBRARY=/path/to/lib2000 python3 crates/nalanda-desktop/bench/bench.py 7 \
  "gpui=150=exec target/release/nalanda-gpui" "rune=277=exec target/release/nalanda"
```
