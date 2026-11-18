# Glide Phone ↔ glide-phone-desktop feature matrix

Both apps speak the KDE Connect LAN protocol (v8, v7 compatible). The **desktop also works
with KDE Connect Android**; the **Glide Phone app pairs only with Glide desktops** (they
announce `glideProtocol` in their identity). Glide-only packets are prefixed `glide.` and are
ignored by KDE Connect.

Every feature below is implemented in **both** apps: each one does its own end of it. "Both
ways" means it also works in either direction; where a feature has one direction, the last
column says why the other doesn't exist (in KDE Connect either).

| Feature | Glide Phone app | glide-phone-desktop | Works | Packets |
|---|---|---|---|---|
| Pairing, TLS, discovery (UDP, mDNS on phone, custom hosts) | ✓ | ✓ | both ways | `kdeconnect.identity`, `kdeconnect.pair` |
| Battery (+ low warning) | ✓ sends its level, shows the computer's | ✓ sends its level on every change, shows the phone's | both ways | `kdeconnect.battery` |
| **Charge alerts** (Glide only) | ✓ "computer low / charged" | ✓ "phone charged" | both ways | `kdeconnect.battery` |
| Clipboard (+ newest wins on connect) | ✓ | ✓ automatic with `wl-paste --watch` (needs data-control); on Glide the window's Clipboard button | both ways | `kdeconnect.clipboard(.connect)` |
| Notifications (actions, replies, contact pictures) | ✓ sends and shows | ✓ sends (D-Bus monitor) and shows | both ways | `kdeconnect.notification*` |
| Find my device | ✓ | ✓ | both ways | `kdeconnect.findmyphone.request` |
| Ping | ✓ | ✓ | both ways | `kdeconnect.ping` |
| Share files / text / links (batches, cancel) | ✓ | ✓ (payload server, file picker) | both ways | `kdeconnect.share.request(.update)` |
| Media control | ✓ controls the computer's players, exports its own | ✓ controls the phone's players (as MPRIS), exports its own | both ways | `kdeconnect.mpris(.request)` |
| Volume | ✓ sets the computer's outputs; **offers its own streams** (Glide only) | ✓ offers its outputs (`pactl`); **sets the phone's media / ringer / notification / alarm volume** (Glide only) | both ways | `kdeconnect.systemvolume(.request)` |
| Mouse & keyboard | ✓ drives the computer (touchpad, keys, typing); takes the computer's input (accessibility + IME) | ✓ takes the phone's input (`/dev/uinput`, Wayland virtual keyboard where offered); drives the phone from the window | both ways | `kdeconnect.mousepad.*` |
| Share input devices (cursor across the screen edge) | ✓ takes the cursor, hands it back at its edge | ✓ the cursor and keyboard go over at the chosen edge (layer-shell strip + pointer lock; Ctrl+Alt+Esc takes it back) | computer → phone: the phone has no mouse to send over (it drives the computer as a touchpad instead) | `kdeconnect.shareinputdevices*` |
| Presenter (pointer, slides, volume keys) | ✓ the remote | ✓ moves slides / shows the pointer | phone → computer: the phone is the hand-held remote | `kdeconnect.presenter` |
| Drawing tablet | ✓ the pen surface | ✓ virtual tablet | phone → computer: the touchscreen is the pen surface | `kdeconnect.digitizer(.session)` |
| Run commands (+ live output, widget, device controls) | ✓ lists and runs, shows output | ✓ keeps commands, runs them, streams output | phone → computer: Android has no shell commands to run | `kdeconnect.runcommand*` |
| Telephony (calls, mute, pause media) | ✓ reports calls, mutes on request | ✓ shows calls, mutes the ringer, pauses media | both ends; calls only happen on the phone | `kdeconnect.telephony*` |
| Connectivity (signal per SIM) | ✓ reports | ✓ shows | phone → computer: the computer has no SIM | `kdeconnect.connectivity_report` |
| Contacts | ✓ sends its address book | ✓ keeps it (`~/.local/share/kpeoplevcard`), names callers and SMS | phone → computer: the address book lives on the phone | `kdeconnect.contacts.*` |
| SMS (no MMS) | ✓ serves conversations, sends | ✓ shows conversations, replies, writes new | both ends; SMS only exist on the phone | `kdeconnect.sms.*` |
| Files | ✓ serves its storage over SFTP; **browses the computer's home folder and downloads from it** (Glide only) | ✓ browses the phone (`sshfs` / `sftp://`); **lets the phone browse its home folder** (Glide only, off until allowed; no hidden files, no way out of home) | both ways | `kdeconnect.sftp(.request)`, `glide.files(.request)` |
| **Wake-on-LAN** (Glide only) | ✓ wakes a sleeping computer | ✓ tells the phone its MACs | phone → computer: phones can't be woken over the network | `glide.wol` |
| **Screenshot sync** (Glide only) | ✓ sends new screenshots (opt-in); receives the computer's | ✓ sends new screenshots (opt-in, Pictures folder); receives the phone's | both ways | `kdeconnect.share.request` |

## Not done

- MMS (by decision), Bluetooth backend, SFTP on Android 7.
- The top-bar applet lives in glide-shell; `the_top_bar_applet_sees_every_battery_change` checks
  it against our ListDevicesJson by its own parsing rules.

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

**Live, the real Glide Phone app ↔ this daemon** (`scripts/real-app-live-test.sh` in the phone
repo; fresh install on an emulator, private D-Bus session): pairing from the app's onboarding with
the code confirmed on both sides, a battery change, a contact's picture arriving as the
notification icon (not the app's), the phone's volume set from the computer and its volume button
seen on the computer, the phone browsing this computer's files (refused while off) and
downloading one, a screenshot taken here reaching the phone, and this computer's cursor going over
to the phone at the right edge, moving the phone's cursor and coming back at the phone's edge. Found and fixed: every trusted
link closing on a `DevicePlugin` NullPointerException (phone), flat avatars taken for app glyphs
(desktop).

**Top-bar applet**: watched on the live session bus, the real applet (the `glide` process) reads
`ListDevicesJson` every 2 s; `the_top_bar_applet_sees_every_battery_change` parses our answers by its
rules. The stale battery came from two daemons running at once in that session (one from autostart,
one from D-Bus activation), which the new daemon prevents. Not shown on screen: Glide has no nested
mode and screen capture needs the user.

**Live, against the real KDE Connect Android** (1.37, built from source; `scripts/kde-connect-live-test.sh`
in this repo, emulator ↔ this daemon in a private D-Bus session, passes repeatedly): connection over
TLS, pairing requested by KDE Connect and accepted here (and the other way round, by hand), its
battery and signal, its notifications, ping and a file from this computer, and KDE Connect running a
command here, and KDE Connect's notification pictures (fetched through `adb forward`). This run
found and fixed the D-Bus `ShareFiles` panic.

**Live, between the two real implementations** (`scripts/live-desktop-test.sh` in the phone repo:
emulator ↔ this daemon in a private D-Bus session): glideProtocol marker, pairing (phone asks,
computer accepts), the computer's greeting (commands, contacts request, glide.wol, keyboard
state), MPRIS / system volume / run command requests and answers, the phone's battery shown on
the computer, and computer → phone ping, shared text and typed text.


Desktop: `cargo test` (unit + integration on a private D-Bus bus: MPRIS both ways, notification
monitor, payload server, run command output), `cargo clippy --all-targets` clean; uinput device
and tablet creation checked by hand (`--ignored` tests), as is the cursor going over at the edge
on the Glide compositor (`pushing_into_the_edge_takes_the_cursor_until_released`, virtual mouse). Phone: unit tests, lint, 42 instrumented
tests on an emulator (loopback protocol tests, touchpad, digitizer, media notification, …), CI
build on GitHub (branch builds and a tag release). Not yet exercised on a physical phone.
