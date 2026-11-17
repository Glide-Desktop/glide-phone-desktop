# Glide Phone Link (desktop)

The desktop side of Phone Link for the [Glide desktop](https://github.com/Glide-Desktop):
the `glide-phone` daemon speaks the KDE Connect LAN protocol and has its own
window. It works with the [Glide Phone](https://github.com/Glide-Desktop/glide-phone)
Android app and with KDE Connect Android: notifications both ways, files and links
both ways, clipboard, media control both ways (MPRIS), system volume, run commands,
the phone as touchpad / keyboard / drawing tablet, the computer controlling the phone,
SFTP browsing of the phone, calls, SMS, contacts, battery and signal, find my device,
plus Glide-only extras (Wake-on-LAN, charge alerts). See [docs/FEATURES.md](docs/FEATURES.md).

Optional at run time: `sshfs` (phone files), `pactl` (volume), access to `/dev/uinput`
(given to the session user by `uaccess`; the phone as mouse, keyboard and tablet).

## Install this checkout

```sh
scripts/install-local.sh   # builds, installs over /usr/bin/glide-phone, restarts the daemon
```

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
