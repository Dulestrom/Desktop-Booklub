# Building Booklub

Booklub is written in Rust with GTK4 and libadwaita, which also run on Windows and macOS. The steps below build it from source on each.

| Platform | Status |
|---|---|
| Ubuntu 26.04 | Supported, tested, and packaged as a `.deb` |
| Windows 10 or 11 | Should build through MSYS2, but **has not been tested yet** |
| macOS | Should build through Homebrew, but **has not been tested yet** |

If you try Windows or macOS, please open an issue saying what worked and what didn't. That will help turn these untested notes into supported instructions.

## Ubuntu

```bash
sudo apt install rustc cargo libgtk-4-dev libadwaita-1-dev libsqlite3-dev pkg-config build-essential fonts-manrope
```

```bash
cargo run -p booklub-app --release
```

To build an installable package:

```bash
cargo install cargo-deb
```

```bash
tools/build_release.sh
```

## Windows

Use [MSYS2](https://www.msys2.org), which provides ready-built GTK4 and libadwaita. Booklub needs GTK 4.22 and libadwaita 1.9 or newer, and when this guide was written, MSYS2 offered GTK 4.24, libadwaita 1.10, and Rust 1.99.

### 1. Install the tools

1. Install MSYS2 from [msys2.org](https://www.msys2.org).
2. Open **MSYS2 UCRT64** from the Start menu. Use this one for every step below, because the other MSYS2 shells target different libraries.
3. Install the packages:

```bash
pacman -Syu
```

MSYS2 may close the window to finish updating. If it does, reopen **MSYS2 UCRT64** and run:

```bash
pacman -S --needed git mingw-w64-ucrt-x86_64-rust mingw-w64-ucrt-x86_64-gtk4 mingw-w64-ucrt-x86_64-libadwaita mingw-w64-ucrt-x86_64-sqlite3 mingw-w64-ucrt-x86_64-pkgconf mingw-w64-ucrt-x86_64-gcc
```

Use MSYS2's own Rust rather than one from rustup. Its build target matches the GTK libraries it installs.

### 2. Build and run

```bash
git clone https://github.com/Dulestrom/Desktop-Booklub.git
```

```bash
cd Desktop-Booklub
```

```bash
cargo run -p booklub-app --release
```

### 3. Install the font

Booklub is designed around [Manrope](https://fonts.google.com/specimen/Manrope). On Windows, install it yourself from Google Fonts. Without it, Booklub still works but falls back to another typeface, so spacing will look different.

### 4. Run it outside MSYS2 (optional)

`target/release/booklub.exe` only starts from the UCRT64 shell, because it needs GTK's files from MSYS2. To run it elsewhere, copy these next to a `bin` folder holding the `.exe`:

```bash
mkdir -p dist/bin dist/share/glib-2.0 dist/share/icons dist/lib
```

```bash
cp target/release/booklub.exe dist/bin/
```

```bash
ldd target/release/booklub.exe | grep -i ucrt64 | awk '{print $3}' | xargs -I{} cp {} dist/bin/
```

```bash
cp -r /ucrt64/share/glib-2.0/schemas dist/share/glib-2.0/
```

```bash
cp -r /ucrt64/share/icons/Adwaita /ucrt64/share/icons/hicolor dist/share/icons/
```

```bash
cp -r /ucrt64/lib/gdk-pixbuf-2.0 dist/lib/
```

This list comes from how GTK apps are usually bundled on Windows and hasn't been tried with Booklub. If something is missing, the app will say so when it starts.

### What differs on Windows

- **Your library** is stored in `%LOCALAPPDATA%\booklub`, and its cache in `%LOCALAPPDATA%\Cache\booklub`.
- **There is no installer.** The `.deb`, the application menu entry, and the `.booklub` file icon are Linux only.
- **Following the system's light or dark setting** depends on GTK and libadwaita on Windows, so it may behave differently from Ubuntu.
- **The development scripts** in `tools/` assume Linux.

### Why not Visual Studio?

GTK can also be built for Rust's MSVC toolchain using [gvsbuild](https://github.com/wingtk/gvsbuild), but that means compiling the whole GTK stack yourself. MSYS2 is much the shorter route.

## macOS

This should be similar, using [Homebrew](https://brew.sh):

```bash
brew install rust gtk4 libadwaita sqlite pkg-config
```

```bash
cargo run -p booklub-app --release
```

Install Manrope from Google Fonts as well. Your library is stored in `~/.local/share/booklub`.

## Tests

```bash
cargo test --workspace
```

`tools/README.md` covers the end-to-end run, which drives the real window. It needs a Linux desktop.
