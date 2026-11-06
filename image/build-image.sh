#!/bin/bash
#
# Build the Sentry USB SD card image for Raspberry Pi Imager.
#
# Starts from the newest official Raspberry Pi OS Lite (64-bit) image,
# installs one Sentry USB release into it with install-pi.sh, and writes:
#
#   Sentry-USB-for-Raspberry-Pi-Imager.rpi-imager-manifest   users open this
#   sentryusb-raspberry-pi-image.img.xz                      the image
#   sentryusb-raspberry-pi-image.img.xz.sha256               its checksum
#   sentryusb-imager-os-list.json                            the same entry for
#                                                            Raspberry Pi's own list
#
# GitHub lists release files by name, ignoring case. The manifest's
# "Sentry-USB-" name puts it above everything; the image sorts in among
# the sentryusb-* binaries, so people open the manifest rather than flash
# the image without Imager's Wi-Fi and user settings.
#
# Pi OS's first boot is left as it ships: it applies the settings chosen in
# Imager (Wi-Fi, user, hostname, SSH), gives the card its own disk ID and
# grows the root partition. Setup then shrinks root exactly as it does for
# a one-line install.
#
# Usage (Linux, as root, on an arm64 host or one with qemu-aarch64 binfmt):
#   image/build-image.sh --tag v3.29.0 [--out DIR] [--installer FILE]

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="${REPO:-Sentry-Six/Sentry-USB-Rusty}"
TAG=""
OUT_DIR="$PWD/deploy"
INSTALLER="$SCRIPT_DIR/../install-pi.sh"
OS_LIST="https://downloads.raspberrypi.com/os_list_imagingutility_v4.json"
GROW_MB=2048        # room to upgrade and install before shrinking back
HEADROOM_MB=300     # free space left in root for the first boot

IMAGE_NAME="sentryusb-raspberry-pi-image.img"
MANIFEST_NAME="Sentry-USB-for-Raspberry-Pi-Imager.rpi-imager-manifest"
OS_LIST_NAME="sentryusb-imager-os-list.json"

# Tools setup and the installer would otherwise fetch on the user's Pi.
# dnsmasq/hostapd are deliberately absent: setup only needs them without
# NetworkManager, and enabled they would take over the Pi's networking.
PACKAGES="autofs avahi-daemon bluez curl dos2unix dosfstools ethtool exfatprogs
fdisk iw jq net-tools netcat-openbsd ntpsec-ntpdig parted pi-bluetooth
python3-dbus python3-gi rfkill rsync util-linux wireless-tools xfsprogs zip"

info()  { echo "[image] $*"; }
fail()  { echo "[image] ERROR: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --tag)       TAG="$2"; shift 2 ;;
        --out)       OUT_DIR="$2"; shift 2 ;;
        --installer) INSTALLER="$2"; shift 2 ;;
        --os-list)   OS_LIST="$2"; shift 2 ;;
        *)           fail "unknown argument: $1" ;;
    esac
done

[ -n "$TAG" ] || fail "--tag is required (the Sentry USB release to install)"
[ "$(id -u)" = 0 ] || fail "run as root"
[ -f "$INSTALLER" ] || fail "installer not found: $INSTALLER"
grep -q 'SENTRYUSB_IMAGE_BUILD' "$INSTALLER" || fail "$INSTALLER has no image build mode"
for tool in chroot curl dumpe2fs e2fsck losetup python3 resize2fs sfdisk truncate xz zerofree; do
    command -v "$tool" >/dev/null || fail "missing tool: $tool"
done
if [ "$(uname -m)" != aarch64 ] && [ ! -e /proc/sys/fs/binfmt_misc/qemu-aarch64 ]; then
    fail "needs an arm64 host, or qemu-user-static with binfmt for aarch64"
fi

mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sentryusb-image.XXXXXX")"
ROOT="$WORK/root"
IMG="$WORK/$IMAGE_NAME"
LOOP=""

cleanup() {
    set +e
    if [ -n "$ROOT" ] && mountpoint -q "$ROOT"; then
        for m in dev/pts dev proc sys boot/firmware; do
            mountpoint -q "$ROOT/$m" && umount -l "$ROOT/$m"
        done
        umount -l "$ROOT"
    fi
    [ -n "$LOOP" ] && losetup -d "$LOOP"
    rm -rf "$WORK"
}
trap cleanup EXIT

attach() {
    LOOP="$(losetup -fP --show "$IMG")"
    udevadm settle 2>/dev/null || true
    [ -b "${LOOP}p2" ] || partx -a "$LOOP" 2>/dev/null || true
    [ -b "${LOOP}p1" ] && [ -b "${LOOP}p2" ] || fail "no partitions on $LOOP"
}

detach() {
    losetup -d "$LOOP"
    LOOP=""
}

fsck_root() {
    # e2fsck exits 1 when it fixed something, which is fine here.
    e2fsck -fy "${LOOP}p2" || [ $? -le 1 ] || fail "e2fsck failed on the root filesystem"
}

# ── 1. Base image ─────────────────────────────────────────────────────

info "Looking up the base image in Raspberry Pi's Imager OS list..."
python3 -I "$SCRIPT_DIR/imager.py" base --os-list "$OS_LIST" --out "$WORK/base.json"
base_field() { python3 -I "$SCRIPT_DIR/imager.py" field --base "$WORK/base.json" "$1"; }
BASE_URL="$(base_field url)"
BASE_SHA="$(base_field extract_sha256)"
BASE_DATE="$(base_field release_date)"
info "Base: $BASE_URL ($(base_field init_format))"

curl -fL --retry 3 --retry-delay 5 -o "$WORK/base.img.xz" "$BASE_URL"
xz -dc "$WORK/base.img.xz" > "$IMG"
rm -f "$WORK/base.img.xz"
echo "$BASE_SHA  $IMG" | sha256sum -c --quiet - || fail "base image checksum mismatch"

DISK_ID="$(sfdisk --disk-id "$IMG")"
ROOT_START="$(sfdisk -J "$IMG" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["partitiontable"]["partitions"][1]["start"])')"

# ── 2. Grow root so the install fits ──────────────────────────────────

info "Growing the root partition by ${GROW_MB} MB..."
truncate -s "+${GROW_MB}M" "$IMG"
echo ', +' | sfdisk --no-reread --no-tell-kernel -q -N 2 "$IMG"
attach
fsck_root
resize2fs "${LOOP}p2"

# ── 3. Mount and prepare the chroot ───────────────────────────────────

mkdir -p "$ROOT"
mount "${LOOP}p2" "$ROOT"
mount "${LOOP}p1" "$ROOT/boot/firmware"
mount --bind /dev "$ROOT/dev"
mount --bind /dev/pts "$ROOT/dev/pts"
mount -t proc proc "$ROOT/proc"
mount -t sysfs sys "$ROOT/sys"

# State the first boot depends on. machine-id must stay "uninitialized" (or
# absent) or systemd skips its first-boot units, and every card must make
# its own SSH host keys. Package installs can create both.
STATE="$WORK/state"
mkdir -p "$STATE"
cp -a "$ROOT/etc/machine-id" "$STATE/machine-id" 2>/dev/null || true
cp -a "$ROOT/var/lib/dbus/machine-id" "$STATE/dbus-machine-id" 2>/dev/null || true
find "$ROOT/etc/ssh" -maxdepth 1 -name 'ssh_host_*' -printf '%f\n' 2>/dev/null | sort > "$STATE/ssh-keys"

mv "$ROOT/etc/resolv.conf" "$STATE/resolv.conf" 2>/dev/null || true
cp -L /etc/resolv.conf "$ROOT/etc/resolv.conf"
printf '#!/bin/sh\nexit 101\n' > "$ROOT/usr/sbin/policy-rc.d"
chmod 755 "$ROOT/usr/sbin/policy-rc.d"

in_chroot() {
    chroot "$ROOT" /usr/bin/env -i \
        PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
        HOME=/root LANG=C.UTF-8 DEBIAN_FRONTEND=noninteractive "$@"
}

# ── 4. Update Pi OS and install Sentry USB ────────────────────────────

info "Updating Raspberry Pi OS..."
in_chroot apt-get update
in_chroot apt-get -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold full-upgrade
# shellcheck disable=SC2086
in_chroot apt-get -y install $PACKAGES

info "Installing Sentry USB $TAG..."
install -m 755 "$INSTALLER" "$ROOT/tmp/install-pi.sh"
in_chroot SENTRYUSB_IMAGE_BUILD=1 SENTRYUSB_RELEASE_TAG="$TAG" REPO="$REPO" bash /tmp/install-pi.sh

cat > "$ROOT/etc/sentryusb-image" <<EOF
SENTRYUSB_IMAGE_VERSION=$TAG
SENTRYUSB_IMAGE_BASE=$(basename "$BASE_URL")
SENTRYUSB_IMAGE_BUILT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF

# ── 5. Clean up and restore first-boot state ──────────────────────────

info "Cleaning up..."
in_chroot apt-get clean
rm -f "$ROOT/usr/sbin/policy-rc.d" "$ROOT/tmp/install-pi.sh" "$ROOT/etc/resolv.conf"
mv "$STATE/resolv.conf" "$ROOT/etc/resolv.conf" 2>/dev/null || true
rm -rf "$ROOT/tmp/"* "$ROOT/var/tmp/"* "$ROOT/root/.bash_history"
find "$ROOT/var/log" -type f -exec truncate -s 0 {} +
find "$ROOT/var/cache/debconf" -name '*-old' -delete

if [ -e "$STATE/machine-id" ]; then
    cp -a "$STATE/machine-id" "$ROOT/etc/machine-id"
else
    rm -f "$ROOT/etc/machine-id"
fi
if [ -e "$STATE/dbus-machine-id" ] || [ -L "$STATE/dbus-machine-id" ]; then
    rm -f "$ROOT/var/lib/dbus/machine-id"
    cp -a "$STATE/dbus-machine-id" "$ROOT/var/lib/dbus/machine-id"
else
    rm -f "$ROOT/var/lib/dbus/machine-id"
fi
for key in "$ROOT"/etc/ssh/ssh_host_*; do
    [ -e "$key" ] || continue
    grep -qxF "$(basename "$key")" "$STATE/ssh-keys" || rm -f "$key"
done

for m in dev/pts dev proc sys boot/firmware; do
    umount "$ROOT/$m"
done
umount "$ROOT"

# ── 6. Shrink back to content ─────────────────────────────────────────

info "Shrinking the image..."
fsck_root
resize2fs -M "${LOOP}p2"
BLOCK_SIZE="$(dumpe2fs -h "${LOOP}p2" 2>/dev/null | awk -F: '/^Block size/ {gsub(/ /,"",$2); print $2}')"
BLOCKS="$(dumpe2fs -h "${LOOP}p2" 2>/dev/null | awk -F: '/^Block count/ {gsub(/ /,"",$2); print $2}')"
BLOCKS=$((BLOCKS + HEADROOM_MB * 1024 * 1024 / BLOCK_SIZE))
resize2fs "${LOOP}p2" "$BLOCKS"
zerofree "${LOOP}p2"
detach

ROOT_SECTORS=$((BLOCKS * BLOCK_SIZE / 512))
echo ", $ROOT_SECTORS" | sfdisk --no-reread --no-tell-kernel -q -N 2 "$IMG"
truncate -s $(((ROOT_START + ROOT_SECTORS) * 512)) "$IMG"

[ "$(sfdisk --disk-id "$IMG")" = "$DISK_ID" ] || fail "disk ID changed; root=PARTUUID would not match"
NEW_START="$(sfdisk -J "$IMG" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["partitiontable"]["partitions"][1]["start"])')"
[ "$NEW_START" = "$ROOT_START" ] || fail "root partition moved ($ROOT_START -> $NEW_START)"

# ── 7. Compress and describe ──────────────────────────────────────────

EXTRACT_SIZE="$(stat -c %s "$IMG")"
EXTRACT_SHA="$(sha256sum "$IMG" | cut -d' ' -f1)"
info "Compressing ($((EXTRACT_SIZE / 1024 / 1024)) MB)..."
xz -T0 -6 -c "$IMG" > "$OUT_DIR/$IMAGE_NAME.xz"
(cd "$OUT_DIR" && sha256sum "$IMAGE_NAME.xz" > "$IMAGE_NAME.xz.sha256")

python3 -I "$SCRIPT_DIR/imager.py" manifest \
    --base "$WORK/base.json" \
    --image "$OUT_DIR/$IMAGE_NAME.xz" \
    --url "https://github.com/$REPO/releases/download/$TAG/$IMAGE_NAME.xz" \
    --extract-size "$EXTRACT_SIZE" \
    --extract-sha256 "$EXTRACT_SHA" \
    --version "$TAG" \
    --release-date "$(date -u +%F)" \
    --icon "https://raw.githubusercontent.com/$REPO/main/image/icon.png" \
    --out "$OUT_DIR/$MANIFEST_NAME" \
    --os-list-out "$OUT_DIR/$OS_LIST_NAME"

info "Done (base $BASE_DATE):"
ls -l "$OUT_DIR/$MANIFEST_NAME" "$OUT_DIR/$OS_LIST_NAME" "$OUT_DIR/$IMAGE_NAME.xz" "$OUT_DIR/$IMAGE_NAME.xz.sha256"
