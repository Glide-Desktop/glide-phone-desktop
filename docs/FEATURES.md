# Glide Phone ↔ glide-phone-desktop feature matrix

Both apps speak the KDE Connect LAN protocol (v8, v7 compatible). The **desktop also works
with KDE Connect Android**; the **Glide Phone app pairs only with Glide desktops** (they
announce `glideProtocol` in their identity). Glide-only packets are prefixed `glide.` and are
ignored by KDE Connect.

| Feature | Phone → computer | Computer → phone | Packets |
|---|---|---|---|
| Pairing, TLS, discovery (UDP, mDNS on phone, custom hosts) | ✓ | ✓ | `kdeconnect.identity`, `kdeconnect.pair` |
| Battery | ✓ phone level, low warning | ✓ computer level on every change | `kdeconnect.battery` |
| Clipboard (+ newest wins on connect) | ✓ | ✓ automatic with `wl-paste --watch` (needs data-control); on Glide the window's Clipboard button | `kdeconnect.clipboard(.connect)` |
| Notifications (actions, replies, icons/avatars) | ✓ phone → desktop popups | ✓ desktop → phone (D-Bus monitor) | `kdeconnect.notification*` |
| Find my device | ✓ ring the computer | ✓ ring the phone | `kdeconnect.findmyphone.request` |
| Ping | ✓ | ✓ | `kdeconnect.ping` |
| Share files / text / links (batches, cancel) | ✓ | ✓ (payload server, file picker) | `kdeconnect.share.request(.update)` |
| Presenter (pointer, slides, volume keys) | ✓ | — (phone is the remote) | `kdeconnect.presenter`, `mousepad.request` |
| Mouse & keyboard | ✓ phone drives the computer (`/dev/uinput`; text via the Wayland virtual keyboard where offered, else key codes / paste) | ✓ computer drives the phone (accessibility + IME) | `kdeconnect.mousepad.*` |
| Drawing tablet | ✓ phone pen → virtual tablet | — | `kdeconnect.digitizer(.session)` |
| Run commands (+ live output, widget, device controls) | ✓ phone runs computer commands | — | `kdeconnect.runcommand*` |
| Media control | ✓ phone controls computer players (MPRIS) | ✓ computer controls phone players (exported as MPRIS) | `kdeconnect.mpris(.request)` |
| System volume | ✓ phone sets computer outputs (`pactl`) | — | `kdeconnect.systemvolume(.request)` |
| Phone files (SFTP) | — | ✓ computer browses the phone (`sshfs` / `sftp://`) | `kdeconnect.sftp(.request)` |
| Telephony (calls, mute, pause media) | ✓ | ✓ mute from the notification | `kdeconnect.telephony*` |
| Connectivity (signal per SIM) | ✓ | — | `kdeconnect.connectivity_report` |
| Contacts | ✓ synced to `~/.local/share/kpeoplevcard` | — | `kdeconnect.contacts.*` |
| SMS (no MMS) | ✓ conversations in the window (new ones arrive as the SMS app's notification) | ✓ reply / send from the computer | `kdeconnect.sms.*` |
| Share input devices (cursor across the screen edge) | ✓ phone side | ✗ needs an InputCapture portal in the Glide compositor | `kdeconnect.shareinputdevices*` |
| **Wake-on-LAN** (Glide only) | ✓ wake a sleeping computer | ✓ computer sends its MACs | `glide.wol` |
| **Screenshot sync** (Glide only) | ✓ new screenshots go to the computer | — | `kdeconnect.share.request` |
| **Charge alerts** (Glide only) | ✓ "laptop low / charged" on the phone | ✓ "phone charged" on the computer | `kdeconnect.battery` |

## Not done

- MMS (by decision), Bluetooth backend, SFTP on Android 7.
- Share input devices from the computer: the compositor needs `org.freedesktop.portal.InputCapture`.
- The top-bar applet lives in the Glide shell repository.

## Hardening (desktop)

A paired phone is trusted with input by design, but not with the rest of the computer:
album art is served only when it is the player's own `mpris:artUrl`; an SFTP login is used only
right after our own request, only from the phone's address, and never opens a path outside the
mount; contacts, SMS threads, media players, run commands, typed text and input events are all
bounded; the payload port waits for the pinned receiver instead of the first connection; nothing
but pairing (not even the battery) reaches an unpaired device.

## Hardening (phone)

Nothing from the network is parsed deeper than 64 brackets (Android's org.json would overflow the
stack and kill the app); SFTP serves only the storage volumes, never the app's own data (host key,
trusted devices, preferences); the computer can act only on notifications it was sent, from apps
still allowed; received files open only on the user's tap (Android 10+); MACs, device types,
media players, album art, downloads and dials before pairing are all bounded.

## Verified

**Live, between the two real implementations** (`scripts/live-desktop-test.sh` in the phone repo:
emulator ↔ this daemon in a private D-Bus session): glideProtocol marker, pairing (phone asks,
computer accepts), the computer's greeting (commands, contacts request, glide.wol, keyboard
state), MPRIS / system volume / run command requests and answers, the phone's battery shown on
the computer, and computer → phone ping, shared text and typed text.


Desktop: `cargo test` (unit + integration on a private D-Bus bus: MPRIS both ways, notification
monitor, payload server, run command output), `cargo clippy --all-targets` clean; uinput device
and tablet creation checked by hand (`--ignored` tests). Phone: unit tests, lint, 42 instrumented
tests on an emulator (loopback protocol tests, touchpad, digitizer, media notification, …), CI
build on GitHub (branch builds and a tag release). Not yet exercised on a physical phone.
