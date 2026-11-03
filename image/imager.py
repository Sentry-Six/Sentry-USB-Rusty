#!/usr/bin/env python3
"""Raspberry Pi Imager metadata for the Sentry USB SD card image.

  base      Find the official Raspberry Pi OS entry the image is built
            from, in Raspberry Pi's Imager OS list, and save it.
  manifest  Write the .rpi-imager-manifest users open in Imager.

Imager 2 offers its customisation screens (Wi-Fi, user, hostname, SSH)
only for an OS whose list entry declares an init_format; a file picked
with "Use custom" gets none. The manifest copies init_format and the
device tags from the base entry, so Imager configures the card exactly
as it would stock Raspberry Pi OS.
"""

import argparse
import hashlib
import json
import os
import sys
import urllib.request

OS_LIST_URL = 'https://downloads.raspberrypi.com/os_list_imagingutility_v4.json'
BASE_NAME = 'Raspberry Pi OS Lite (64-bit)'

# Imager options the manifest turns on. user_required makes Imager ask
# for a user before writing: Pi OS has no default user, and a headless
# card without one stops at a console prompt nobody can see.
CAPABILITIES = ['user_required']

# Raspberry Pi's board descriptions name every model sharing a tag; only
# these can be a USB drive for the car (README "Supported hardware").
DEVICE_DESCRIPTIONS = {
    'Raspberry Pi 5': 'Raspberry Pi 5',
    'Raspberry Pi 4': 'Raspberry Pi 4 Model B',
    'Raspberry Pi 3': 'Raspberry Pi 3 Model A+ only. The 3B and 3B+ can\'t '
                      'act as a USB drive for the car.',
}


def find_entry(os_list, name):
    for entry in os_list:
        if 'subitems' in entry:
            found = find_entry(entry['subitems'], name)
            if found:
                return found
        elif entry.get('name') == name and 'url' in entry:
            return entry
    return None


def cmd_base(args):
    if os.path.exists(args.os_list):
        with open(args.os_list) as f:
            data = json.load(f)
    else:
        with urllib.request.urlopen(args.os_list, timeout=60) as resp:
            data = json.load(resp)
    entry = find_entry(data.get('os_list', []), args.name)
    if entry is None:
        sys.exit(f'"{args.name}" is not in {args.os_list}')
    for key in ('url', 'extract_sha256', 'init_format', 'devices'):
        if not entry.get(key):
            sys.exit(f'"{args.name}" has no {key} in {args.os_list}')
    tags = set(entry['devices'])
    # Same device filter as Imager's own doc/local_json/create_local_json.py.
    devices = [
        d for d in data.get('imager', {}).get('devices', [])
        if not d.get('tags') or tags.intersection(d['tags'])
    ]
    for d in devices:
        if d.get('name') in DEVICE_DESCRIPTIONS:
            d['description'] = DEVICE_DESCRIPTIONS[d['name']]
    with open(args.out, 'w') as f:
        json.dump({'entry': entry, 'devices': devices}, f, indent=2)
    if args.field:
        print(entry[args.field])


def cmd_field(args):
    with open(args.base) as f:
        print(json.load(f)['entry'][args.field])


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()


def cmd_manifest(args):
    with open(args.base) as f:
        base = json.load(f)
    entry = base['entry']
    based_on = entry['name']
    if entry.get('release_date'):
        based_on += f', {entry["release_date"]}'
    os_entry = {
        'name': f'Sentry USB {args.version}',
        'description': (
            'Sentry USB for Raspberry Pi 5, 4, 3A+ and Zero 2 W. Set your '
            'Wi-Fi in the next steps, then open http://sentryusb.local. '
            f'Based on {based_on}.'
        ),
        'icon': args.icon,
        'url': args.url,
        'extract_size': args.extract_size,
        'extract_sha256': args.extract_sha256,
        'image_download_size': os.path.getsize(args.image),
        'image_download_sha256': sha256_file(args.image),
        'release_date': args.release_date,
        'init_format': entry['init_format'],
        'devices': entry['devices'],
        'capabilities': CAPABILITIES,
    }
    manifest = {'imager': {'devices': base['devices']}, 'os_list': [os_entry]}
    with open(args.out, 'w') as f:
        json.dump(manifest, f, indent=2)
        f.write('\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='cmd', required=True)

    p = sub.add_parser('base', help='save the base OS entry')
    p.add_argument('--os-list', default=OS_LIST_URL, help='URL or file')
    p.add_argument('--name', default=BASE_NAME)
    p.add_argument('--out', required=True)
    p.add_argument('--field', help='also print this field of the entry')
    p.set_defaults(func=cmd_base)

    p = sub.add_parser('field', help='print a field of a saved base entry')
    p.add_argument('--base', required=True)
    p.add_argument('field')
    p.set_defaults(func=cmd_field)

    p = sub.add_parser('manifest', help='write the Imager manifest')
    p.add_argument('--base', required=True)
    p.add_argument('--image', required=True, help='the .img.xz users download')
    p.add_argument('--url', required=True, help='where Imager downloads it')
    p.add_argument('--extract-size', type=int, required=True)
    p.add_argument('--extract-sha256', required=True)
    p.add_argument('--version', required=True)
    p.add_argument('--release-date', required=True)
    p.add_argument('--icon', required=True)
    p.add_argument('--out', required=True)
    p.set_defaults(func=cmd_manifest)

    args = parser.parse_args()
    args.func(args)


if __name__ == '__main__':
    main()
