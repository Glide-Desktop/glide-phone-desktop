# Glide Phone ↔ glide-phone-desktop feature matrix

Both apps speak the KDE Connect LAN protocol (v8, v7 compatible). The **desktop also works
with KDE Connect Android**; the **Glide Phone app pairs only with Glide desktops** (they
announce `glideProtocol` in their identity). Glide-only packets are prefixed `glide.` and are
ignored by KDE Connect.

| Feature | Phone → computer | Computer → phone | Packets |
|---|---|---|---|
| Pairing, TLS, discovery (UDP, mDNS on phone, custom hosts) | ✓ | ✓ | `kdeconnect.identity`, `kdeconnect.pair` |
| Battery | ✓ phone level, low warning | ✓ computer level on every change | `kdeconnect.battery` |
| Clipboard (+ newest wins on connect) | ✓ | ✓ (`wl-paste --watch`, needs data-control) | `kdeconnect.clipboard(.connect)` |
| Notifications (actions, replies, icons/avatars) | ✓ phone → desktop popups | ✓ desktop → phone (D-Bus monitor) | `kdeconnect.notification*` |
| Find my device | ✓ ring the computer | ✓ ring the phone | `kdeconnect.findmyphone.request` |
| Ping | ✓ | ✓ | `kdeconnect.ping` |
| Share files / text / links (batches, cancel) | ✓ | ✓ (payload server, file picker) | `kdeconnect.share.request(.update)` |
| Presenter (pointer, slides, volume keys) | ✓ | — (phone is the remote) | `kdeconnect.presenter`, `mousepad.request` |
| Mouse & keyboard | ✓ phone drives the computer (`/dev/uinput`) | ✓ computer drives the phone (accessibility + IME) | `kdeconnect.mousepad.*` |
| Drawing tablet | ✓ phone pen → virtual tablet | — | `kdeconnect.digitizer(.session)` |
| Run commands (+ live output, widget, device controls) | ✓ phone runs computer commands | — | `kdeconnect.runcommand*` |
| Media control | ✓ phone controls computer players (MPRIS) | ✓ computer controls phone players (exported as MPRIS) | `kdeconnect.mpris(.request)` |
| System volume | ✓ phone sets computer outputs (`pactl`) | — | `kdeconnect.systemvolume(.request)` |
| Phone files (SFTP) | — | ✓ computer browses the phone (`sshfs` / `sftp://`) | `kdeconnect.sftp(.request)` |
| Telephony (calls, mute, pause media) | ✓ | ✓ mute from the notification | `kdeconnect.telephony*` |
| Connectivity (signal per SIM) | ✓ | — | `kdeconnect.connectivity_report` |
| Contacts | ✓ synced to `~/.local/share/kpeoplevcard` | — | `kdeconnect.contacts.*` |
| SMS (no MMS) | ✓ conversations, new-message notifications | ✓ reply / send from the computer | `kdeconnect.sms.*` |
| Share input devices (cursor across the screen edge) | ✓ phone side | ✗ needs an InputCapture portal in the Glide compositor | `kdeconnect.shareinputdevices*` |
| **Wake-on-LAN** (Glide only) | ✓ wake a sleeping computer | ✓ computer sends its MACs | `glide.wol` |
| **Screenshot sync** (Glide only) | ✓ new screenshots go to the computer | — | `kdeconnect.share.request` |
| **Charge alerts** (Glide only) | ✓ "laptop low / charged" on the phone | ✓ "phone charged" on the computer | `kdeconnect.battery` |

## Not done

- MMS (by decision), Bluetooth backend, SFTP on Android 7.
- Share input devices from the computer: the compositor needs `org.freedesktop.portal.InputCapture`.
- The top-bar applet lives in the Glide shell repository.

## Verified

Desktop: `cargo test` (unit + integration on a private D-Bus bus: MPRIS both ways, notification
monitor, payload server, run command output), `cargo clippy --all-targets` clean; uinput device
and tablet creation checked by hand (`--ignored` tests). Phone: unit tests, lint, 42 instrumented
tests on an emulator (loopback protocol tests, touchpad, digitizer, media notification, …), CI
build on GitHub. **Not yet exercised live between a real phone and this desktop.**
