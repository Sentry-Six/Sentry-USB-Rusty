#!/bin/bash
# Builds a miniature SentryUSB SSD image for disk_reader integration tests.
# Linux + root only (losetup, mkfs.xfs). Mirrors the real layout written by
# crates/setup/src/partition.rs and crates/usb_gadget/src/snapshot.rs:
#   GPT: p1 ext4 "mutable", p2 XFS reflink=1 "backingfiles"
#   backingfiles: cam_disk.bin (live) + snapshots/snap-NNNNNN/{snap.bin,snap.bin.toc}
#
# Outputs:
#   $1               golden disk image (default golden.img, sparse)
#   $1.manifest      "<sha256>  <merged CAM-relative path>" of the EXPECTED
#                    merged view (newest source wins), generated from the
#                    kernel-mounted trees — i.e. ground truth for byte-exact
#                    comparison.
set -euo pipefail

OUT=${1:-golden.img}
MANIFEST="$OUT.manifest"

[ "$(uname)" = Linux ] || { echo "Linux only" >&2; exit 1; }
[ "$(id -u)" = 0 ] || { echo "must run as root" >&2; exit 1; }

WORK=$(mktemp -d)
BF="$WORK/backingfiles"
CM="$WORK/cam"
mkdir -p "$BF" "$CM"
LOOPS=()
detach_loop() {
    local device=$1 candidate
    local keep=()
    losetup -d "$device" || return
    # Detach is asynchronous; release backing-file references before parent unmount.
    udevadm settle --timeout=30 || return
    for candidate in "${LOOPS[@]}"; do
        if [ "$candidate" != "$device" ]; then keep+=("$candidate"); fi
    done
    LOOPS=("${keep[@]}")
}

cleanup() {
    set +e
    if mountpoint -q "$CM" && ! umount "$CM"; then return 1; fi
    for device in "${LOOPS[@]}"; do
        if [ "$device" != "${LOOP:-}" ]; then detach_loop "$device" || return 1; fi
    done
    if mountpoint -q "$BF" && ! umount "$BF"; then return 1; fi
    for device in "${LOOPS[@]}"; do detach_loop "$device" || return 1; done
    if mountpoint -q "$CM" || mountpoint -q "$BF"; then return 1; fi
    rm -rf "$WORK"
}
trap cleanup EXIT

# --- outer disk: GPT with mutable (ext4) + backingfiles (XFS reflink) ------
rm -f "$OUT"
truncate -s 2G "$OUT"
parted --script "$OUT" \
    mklabel gpt \
    mkpart mutable ext4 1MiB 65MiB \
    mkpart backingfiles xfs 65MiB 100%
LOOP=$(losetup -f --show -P "$OUT")
LOOPS+=("$LOOP")
mkfs.ext4 -q -L mutable "${LOOP}p1"
# Same flags as crates/setup/src/partition.rs
mkfs.xfs -q -f -K -m reflink=1 -L backingfiles "${LOOP}p2"
mount "${LOOP}p2" "$BF"

# --- helpers ---------------------------------------------------------------
make_cam_image() { # $1 = image path
    truncate -s 192M "$1"
    echo 'type=c' | sfdisk -q "$1"
    local cl
    cl=$(losetup -f --show -P "$1")
    LOOPS+=("$cl")
    mkfs.vfat -F 32 -n CAM "${cl}p1" >/dev/null
    detach_loop "$cl"
}

mount_cam() { # $1 = image path
    CAMLOOP=$(losetup -f --show -P "$1")
    LOOPS+=("$CAMLOOP")
    mount "${CAMLOOP}p1" "$CM"
}

umount_cam() {
    umount "$CM"
    detach_loop "$CAMLOOP"
}

add_clip() { # $1 = dir under CAM root, $2 = filename, $3 = KiB
    mkdir -p "$CM/$1"
    dd if=/dev/urandom of="$CM/$1/$2" bs=1024 count="$3" status=none
}

write_toc() { # $1 = mounted cam root, $2 = toc path
    (cd "$CM" && find . -type f -printf '%s %P\n') > "$2"
}

snapshot() { # $1 = snap number
    local dir="$BF/snapshots/snap-$(printf '%06d' "$1")"
    mkdir -p "$dir"
    cp --reflink=always "$BF/cam_disk.bin" "$dir/snap.bin"
    mount_cam "$dir/snap.bin"
    write_toc "$CM" "$dir/snap.bin.toc"
    umount_cam
}

# --- content timeline ------------------------------------------------------
# Epoch 1: clip A + event.json  -> snap-000001
make_cam_image "$BF/cam_disk.bin"
mount_cam "$BF/cam_disk.bin"
add_clip "TeslaCam/SavedClips/2026-07-01_10-00-00" "2026-07-01_09-59-30-front.mp4" 700
add_clip "TeslaCam/SavedClips/2026-07-01_10-00-00" "2026-07-01_09-59-30-back.mp4" 650
echo '{"timestamp":"2026-07-01T10:00:00","reason":"sentry_aware_object_detection","est_lat":"12.34","est_lon":"-56.78"}' \
    > "$CM/TeslaCam/SavedClips/2026-07-01_10-00-00/event.json"
umount_cam
snapshot 1

# Epoch 2: clip B added, event.json REWRITTEN (tests newest-wins) -> snap-000002
mount_cam "$BF/cam_disk.bin"
add_clip "TeslaCam/SentryClips/2026-07-02_20-30-00" "2026-07-02_20-29-30-left_repeater.mp4" 500
echo '{"timestamp":"2026-07-01T10:00:00","reason":"sentry_aware_object_detection","est_lat":"12.34","est_lon":"-56.78","camera":"3"}' \
    > "$CM/TeslaCam/SavedClips/2026-07-01_10-00-00/event.json"
umount_cam
snapshot 2

# Epoch 3: clip C only in the live image (no snapshot, no toc)
mount_cam "$BF/cam_disk.bin"
add_clip "TeslaCam/RecentClips" "2026-07-03_08-00-00-front.mp4" 300
umount_cam

# snap-000003: interrupted snapshot — snap.bin but NO toc (exercise exFAT/FAT32
# enumeration fallback)
mkdir -p "$BF/snapshots/snap-000003"
cp --reflink=always "$BF/cam_disk.bin" "$BF/snapshots/snap-000003/snap.bin"

# --- expected merged manifest (ground truth from kernel mounts) -------------
# Merge = union over sources oldest->newest (snap1, snap2, snap3, live),
# newest wins. snap3 == live content, so hashing the live tree covers every
# path; all paths exist in the newest source here by construction.
mount_cam "$BF/cam_disk.bin"
(cd "$CM" && find . -type f -print0 | sort -z | xargs -0 sha256sum | sed 's|  \./|  |') > "$MANIFEST"
umount_cam

umount "$BF"
detach_loop "$LOOP"
trap - EXIT
rm -rf "$WORK"
echo "built $OUT ($(du -h --apparent-size "$OUT" | cut -f1) apparent, $(du -h "$OUT" | cut -f1) real)"
echo "manifest: $MANIFEST ($(wc -l < "$MANIFEST") files)"
