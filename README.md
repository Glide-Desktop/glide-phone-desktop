# Glide Phone Link (desktop)

The desktop side of Phone Link for the [Glide desktop](https://github.com/Glide-Desktop):
the `glide-phone` daemon speaks the KDE Connect LAN protocol (pairing,
battery, notifications, shared files and links, clipboard, find-my-phone) and
has its own window. The Android app lives in
[glide-phone](https://github.com/Glide-Desktop/glide-phone).

## Build

```sh
cargo build --release
```

The shared crates come from a tag of
[glide-libs](https://github.com/Glide-Desktop/glide-libs). To build against a
checkout of glide-libs next to this one, create `.cargo/config.toml` (ignored
by git):

```toml
[patch."https://github.com/Glide-Desktop/glide-libs"]
glide-app = { path = "../glide-libs/crates/glide-app" }
glide-config = { path = "../glide-libs/crates/glide-config" }
glide-desktop = { path = "../glide-libs/crates/glide-desktop" }
glide-paint = { path = "../glide-libs/crates/glide-paint" }
glide-sdk = { path = "../glide-libs/crates/glide-sdk" }
glide-theme = { path = "../glide-libs/crates/glide-theme" }
```

Split from the Glide monorepo with its history in November 2026.
