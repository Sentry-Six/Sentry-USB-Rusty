#!/bin/bash -eu

source /root/bin/envsetup.sh

# Never bind an empty gadget after a recording mount failed to come up.
findmnt -n --mountpoint /backingfiles -O rw >/dev/null || {
  echo "error: recording storage is not mounted read/write"
  exit 1
}
if [[ ! "${CAM_SIZE:-0}" =~ ^0+([.]0+)?([kKmMgG][bB]?)?$ ]] && [ ! -s /backingfiles/cam_disk.bin ]; then
  echo "error: configured camera disk is unavailable"
  exit 1
fi
images=0
for image in cam music lightshow boombox; do
  path="/backingfiles/${image}_disk.bin"
  [ -e "$path" ] || continue
  if [ ! -f "$path" ] || [ ! -s "$path" ] || [ ! -r "$path" ] || [ ! -w "$path" ]; then
    echo "error: USB disk image is not a usable writable file: $path"
    exit 1
  fi
  images=$((images + 1))
done
[ "$images" -gt 0 ] || { echo "error: no USB disk images available"; exit 1; }

if ! configfs_root=$(findmnt -o TARGET -n configfs)
then
  echo "error: configfs not found"
  exit 1
fi
readonly gadget_root="$configfs_root/usb_gadget/sentryusb"

# USB supports many languages. 0x409 is US English
readonly lang=0x409

# configuration name can be anything, the convention
# appears to be to use "c"
readonly cfg=c

if [ -d "$gadget_root" ]
then
  primary=$(cat "$gadget_root/functions/mass_storage.0/lun.0/file" 2>/dev/null || true)
  if [ -n "$(cat "$gadget_root/UDC" 2>/dev/null)" ] && [ -s "$primary" ] &&
     [ "$(cat "$gadget_root/functions/mass_storage.0/lun.0/ro" 2>/dev/null)" = 0 ]; then
    echo "already prepared"
    exit 0
  fi
  echo "error: existing USB gadget is incomplete; disable it before retrying"
  exit 1
fi

modprobe libcomposite

mkdir -p "$gadget_root/configs/$cfg.1"

# common setup
echo 0x1d6b > "$gadget_root/idVendor"  # Linux Foundation
echo 0x0104 > "$gadget_root/idProduct" # Composite Gadget
echo 0x0100 > "$gadget_root/bcdDevice" # v1.0.0
echo 0x0200 > "$gadget_root/bcdUSB"    # USB 2.0
mkdir -p "$gadget_root/strings/$lang"
mkdir -p "$gadget_root/configs/$cfg.1/strings/$lang"
echo "SentryUSB-$(sha256sum < /etc/machine-id | awk '{print $1}')" > "$gadget_root/strings/$lang/serialnumber"
echo SentryUSB > "$gadget_root/strings/$lang/manufacturer"
echo "SentryUSB Composite Gadget" > "$gadget_root/strings/$lang/product"
echo "SentryUSB Config" > "$gadget_root/configs/$cfg.1/strings/$lang/configuration"

# A bare Raspberry Pi 4 or 5 can peak at at over 1 A during boot, but idles around 500 mA.
# A Raspberry Pi 3 can peak at over 500 mA during boot, but idles around 300 mA.
# A Raspberry Pi Zero 2 W can peak at over 300 mA during boot, but idles around 100 mA.
if isPi5
then
  echo 600 > "$gadget_root/configs/$cfg.1/MaxPower"
elif isPi4
then
  echo 500 > "$gadget_root/configs/$cfg.1/MaxPower"
elif isPi3
then
  echo 300 > "$gadget_root/configs/$cfg.1/MaxPower"
elif isPi2
then
  echo 200 > "$gadget_root/configs/$cfg.1/MaxPower"
else
  echo 100 > "$gadget_root/configs/$cfg.1/MaxPower"
fi

# mass storage setup
mkdir -p "$gadget_root/functions/mass_storage.0"

lun=0

# nofua=1 on every LUN: Tesla issues FUA writes, which mass_storage honors as
# synchronous flushes; one slow flush under disk contention can exceed the
# car's SCSI timeout and it drops the drive until re-plugged.
if [ -e "/backingfiles/cam_disk.bin" ]
then
  echo 1 > "$gadget_root/functions/mass_storage.0/lun.${lun}/nofua" || true
  echo "/backingfiles/cam_disk.bin" > "$gadget_root/functions/mass_storage.0/lun.${lun}/file"
  echo "SentryUSB CAM $(du -h /backingfiles/cam_disk.bin | awk '{print $1}')" > "$gadget_root/functions/mass_storage.0/lun.${lun}/inquiry_string"
  ((++lun))
fi

if [ -e "/backingfiles/music_disk.bin" ]
then
  mkdir -p "$gadget_root/functions/mass_storage.0/lun.${lun}"
  echo 1 > "$gadget_root/functions/mass_storage.0/lun.${lun}/nofua" || true
  echo "/backingfiles/music_disk.bin" > "$gadget_root/functions/mass_storage.0/lun.${lun}/file"
  echo "SentryUSB MUSIC $(du -h /backingfiles/music_disk.bin | awk '{print $1}')" > "$gadget_root/functions/mass_storage.0/lun.${lun}/inquiry_string"
  ((++lun))
fi

if [ -e "/backingfiles/lightshow_disk.bin" ]
then
  mkdir -p "$gadget_root/functions/mass_storage.0/lun.${lun}"
  echo 1 > "$gadget_root/functions/mass_storage.0/lun.${lun}/nofua" || true
  echo "/backingfiles/lightshow_disk.bin" > "$gadget_root/functions/mass_storage.0/lun.${lun}/file"
  echo "SentryUSB LIGHTSHOW $(du -h /backingfiles/lightshow_disk.bin | awk '{print $1}')" > "$gadget_root/functions/mass_storage.0/lun.${lun}/inquiry_string"
  ((++lun))
fi

if [ -e "/backingfiles/boombox_disk.bin" ]
then
  mkdir -p "$gadget_root/functions/mass_storage.0/lun.${lun}"
  echo 1 > "$gadget_root/functions/mass_storage.0/lun.${lun}/nofua" || true
  echo "/backingfiles/boombox_disk.bin" > "$gadget_root/functions/mass_storage.0/lun.${lun}/file"
  echo "SentryUSB BOOMBOX $(du -h /backingfiles/boombox_disk.bin | awk '{print $1}')" > "$gadget_root/functions/mass_storage.0/lun.${lun}/inquiry_string"
  ((++lun))
fi

ln -sf "$gadget_root/functions/mass_storage.0" "$gadget_root/configs/$cfg.1"

# activate
primary=$(cat "$gadget_root/functions/mass_storage.0/lun.0/file")
[ -s "$primary" ] || { echo "error: primary USB disk is missing"; exit 1; }
udc=$(find /sys/class/udc -type l -printf '%P\n' | head -1)
[ -n "$udc" ] || { echo "error: no USB device controller available"; exit 1; }
echo "$udc" > "$gadget_root/UDC"
[ "$(cat "$gadget_root/UDC")" = "$udc" ] || { echo "error: USB gadget did not bind"; exit 1; }
