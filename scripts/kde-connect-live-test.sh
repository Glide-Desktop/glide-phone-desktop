#!/usr/bin/env bash
# Checks this daemon against the real KDE Connect Android on an emulator, isolated from the
# user's own daemon (private D-Bus session, config and TCP port):
#   scripts/kde-connect-live-test.sh target/debug/glide-phone path/to/kdeconnect-android-debug.apk
# Build the APK from kdeconnect-android with `ANDROID_HOME=~/Android/Sdk ./gradlew assembleDebug`.
#
# The emulator can't receive the desktop's UDP broadcast, so the identity is sent through
# `adb emu redir`; KDE Connect then dials the daemon at 10.0.2.2. Pairing is requested in KDE
# Connect's UI (uiautomator) and accepted over D-Bus. Note: inbound connections into the
# emulator are impossible, so payloads KDE Connect offers (notification icons) aren't fetched.
set -euo pipefail
bin="$(realpath "${1:?glide-phone binary}")"
apk="${2:?KDE Connect debug APK}"
pkg=org.kde.kdeconnect_tp.debug
adb="${ADB:-$HOME/Android/Sdk/platform-tools/adb} -s ${EMULATOR:-emulator-5554}"
work="$(mktemp -d)"
pid=""
cleanup() {
  [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  $adb emu redir del udp:17160 >/dev/null 2>&1 || true
  $adb shell am force-stop "$pkg" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT
fail() { echo "FAILED: $*"; exit 1; }

ui() { # ui <regex> [tap]: find a node by text and optionally tap it
  python3 - "$@" <<'PY'
import re, subprocess, sys, os, xml.etree.ElementTree as ET
adb = os.environ["ADB_CMD"].split()
subprocess.run(adb + ["shell", "uiautomator", "dump", "/sdcard/ui.xml"], capture_output=True)
root = ET.fromstring(subprocess.run(adb + ["shell", "cat", "/sdcard/ui.xml"], capture_output=True, text=True).stdout)
for n in root.iter("node"):
    if any(re.search(sys.argv[1], v or "") for v in (n.get("text"), n.get("content-desc"))):
        x1, y1, x2, y2 = map(int, re.findall(r"\d+", n.get("bounds")))
        if len(sys.argv) > 2:
            subprocess.run(adb + ["shell", "input", "tap", str((x1 + x2) // 2), str((y1 + y2) // 2)])
        sys.exit(0)
sys.exit(1)
PY
}
export ADB_CMD="$adb"

$adb uninstall "$pkg" >/dev/null 2>&1 || true
$adb install -g "$apk" >/dev/null
$adb shell am force-stop org.mindenit.glidephone # it would hold port 1716 in the emulator
$adb shell cmd notification allow_listener "$pkg/org.kde.kdeconnect.plugins.notifications.NotificationReceiver"
$adb shell monkey -p "$pkg" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
# Start it once more so its notification plugin binds to the listener granted above (it
# checks when loading); before any pairing, so nothing it hasn't saved yet is lost.
sleep 5
$adb shell am force-stop "$pkg"
$adb shell monkey -p "$pkg" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1

mkdir -p "$work/config" "$work/data"
RUST_LOG=glide_phone=info XDG_CONFIG_HOME="$work/config" XDG_DATA_HOME="$work/data" setsid dbus-run-session -- \
  bash -c "echo \$DBUS_SESSION_BUS_ADDRESS > '$work/bus'; exec '$bin' --daemon" > "$work/daemon.log" 2>&1 &
for _ in $(seq 50); do [ -s "$work/bus" ] && [ -s "$work/config/glide/phone.json" ] && break; sleep 0.2; done
sleep 2
bus="$(cat "$work/bus")"
pid="$(pgrep -f -n -- "$bin --daemon")"
port="$(ss -ltnpH | grep "pid=$pid," | awk '{print $4}' | sed 's/.*://' | sort -n | head -1)"
id="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['device_id'])" "$work/config/glide/phone.json")"
call() { busctl --timeout=10 --address="$bus" call org.glide.Phone /org/glide/Phone org.glide.Phone "$@"; }
devices() { busctl --json=short --address="$bus" call org.glide.Phone /org/glide/Phone org.glide.Phone ListDevicesJson |
  python3 -c "import json,sys; print(json.dumps(json.loads(json.load(sys.stdin)['data'][0])))"; }
field() { devices | python3 -c "import json,sys; d=json.load(sys.stdin); print(d[0]$1 if d else '')"; }
echo "desktop $id on port $port"

$adb emu redir add udp:17160:1716 >/dev/null
announce() { # our identity to KDE Connect; it dials back
  python3 - "$id" "$port" <<'PY'
import json, socket, sys, time
body = {"deviceId": sys.argv[1], "deviceName": "glide-test", "deviceType": "desktop", "protocolVersion": 8,
        "tcpPort": int(sys.argv[2]), "incomingCapabilities": [], "outgoingCapabilities": []}
packet = json.dumps({"id": int(time.time() * 1000), "type": "kdeconnect.identity", "body": body}) + "\n"
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(packet.encode(), ("127.0.0.1", 17160))
PY
}
connect() {
  for _ in $(seq 10); do
    announce
    for _ in $(seq 6); do [ "$(field "['connected']")" = True ] && return 0; sleep 0.5; done
  done
  fail "KDE Connect did not connect"
}
sleep 3
connect
phone="$(field "['id']")"
echo "KDE Connect connected as $phone ($(field "['name']"))"

# KDE Connect asks to pair, the computer accepts.
name="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['device_name'])" "$work/config/glide/phone.json")"
for _ in $(seq 20); do ui "^${name}$" tap && break; sleep 1; done
sleep 2
ui '^Request pairing$' tap || fail "no Request pairing button"
for _ in $(seq 30); do [ "$(field "['pair_state']")" = RequestedByPeer ] && break; sleep 0.5; done
[ "$(field "['pair_state']")" = RequestedByPeer ] || fail "the pairing request didn't arrive"
call AcceptPair s "$phone" >/dev/null
for _ in $(seq 30); do [ "$(field "['pair_state']")" = Paired ] && break; sleep 0.5; done
[ "$(field "['pair_state']")" = Paired ] || fail "not paired"
echo "paired"
sleep 5 # KDE Connect loads its plugins once the link is up

# KDE Connect -> computer: battery, signal, notifications.
for _ in $(seq 30); do [ -n "$(field "['battery']")" ] && [ "$(field "['battery']")" != None ] && break; sleep 0.5; done
echo "battery $(field "['battery']"), signal $(field "['signals']")"
[ "$(field "['battery']")" != None ] || fail "no battery from KDE Connect"
$adb shell cmd notification post -t "Glide" glidelive "kde-to-glide" >/dev/null
for _ in $(seq 60); do devices | grep -q kde-to-glide && break; sleep 0.5; done
devices | grep -q kde-to-glide || { devices | cut -c1-600; fail "the notification didn't arrive"; }
echo "notification arrived"

# Computer -> KDE Connect: ping and a file.
call Ping s "$phone" >/dev/null
sleep 2
$adb shell dumpsys notification --noredact | grep -q "Ping!" || fail "no ping on KDE Connect"
echo "ping shown"
echo "glide file $$" > "$work/glide-to-kde-$$.txt"
call ShareFiles sas "$phone" 1 "$work/glide-to-kde-$$.txt" >/dev/null
for _ in $(seq 30); do $adb shell cat "/sdcard/Download/glide-to-kde-$$.txt" 2>/dev/null | grep -q "glide file $$" && break; sleep 0.5; done
$adb shell cat "/sdcard/Download/glide-to-kde-$$.txt" 2>/dev/null | grep -q "glide file $$" || fail "file not received"
$adb shell rm -f "/sdcard/Download/glide-to-kde-$$.txt"
echo "file received"

# KDE Connect runs a computer command.
call AddCommand ss "Marker" "touch '$work/ran'" >/dev/null
# Let the "file received" heads-up go away first: its title is our name too.
sleep 8
$adb shell cmd statusbar collapse >/dev/null 2>&1 || true
$adb shell monkey -p "$pkg" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1 # back to the front
sleep 2
ui '^Run Command$' || { ui "^${name}$" tap || true; sleep 2; } # the device's page
ui '^Run Command$' tap || { $adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1
  $adb shell cat /sdcard/ui.xml | grep -oE 'text="[^"]+"' | head -20; fail "no Run Command"; }
sleep 3
ui '^Marker$' tap || fail "the command isn't listed"
for _ in $(seq 20); do [ -e "$work/ran" ] && break; sleep 0.5; done
if [ ! -e "$work/ran" ]; then
  $adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1
  $adb shell cat /sdcard/ui.xml | grep -oE 'text="[^"]+"' | head -20
  sed 's/\x1b\[[0-9;]*m//g' "$work/daemon.log" | tail -5
  fail "the command didn't run"
fi
echo "command ran"
echo "KDE CONNECT LIVE TEST PASSED"
