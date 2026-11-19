#!/bin/sh
# Builds XilaOS and makes the image of a USB stick that starts it:
# build/xilaos-usb.img. Write the whole image on a stick, start the machine
# from the stick, and type install.
#
# This script never writes on a disk or a stick itself. On macOS, with the
# stick as /dev/diskN (see `diskutil list`; a wrong N erases another disk):
#   diskutil unmountDisk /dev/diskN
#   sudo dd if=build/xilaos-usb.img of=/dev/rdiskN bs=1m
#
# The image is made by the code of `install` itself (core/src/install.rs),
# run on this machine: a partition table, the start-up partition with the
# system in it, and an empty partition.
set -e
cd "$(dirname "$0")/.."
tools/build.sh >/dev/null

if command -v cargo >/dev/null 2>&1; then
    cargo run -q --release -p xila-core --example usb --target-dir target/host -- build/esp/EFI/BOOT/BOOTX64.EFI build/xilaos-usb.img
else
    docker run --rm -v "$PWD":/x -w /x xilaos-build \
        cargo run -q --release -p xila-core --example usb --target-dir target/docker-host -- build/esp/EFI/BOOT/BOOTX64.EFI build/xilaos-usb.img
fi
