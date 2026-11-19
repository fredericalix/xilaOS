#!/bin/sh
# Builds XilaOS and starts it in QEMU, with the serial port on this terminal.
# Extra arguments go to QEMU, for example: tools/run.sh -display none
#
# The tables are kept on a disk, the file build/data.img: they are there
# again at the next start. Remove the file to start from nothing.
set -e
cd "$(dirname "$0")/.."
tools/build.sh >/dev/null
[ -f build/data.img ] || dd if=/dev/zero of=build/data.img bs=1m count=0 seek=64 2>/dev/null

firmware=${XILA_FIRMWARE:-$(dirname "$(dirname "$(command -v qemu-system-x86_64)")")/share/qemu/edk2-x86_64-code.fd}
exec qemu-system-x86_64 -machine q35 -m 256 -net none \
    -drive if=pflash,format=raw,readonly=on,file="$firmware" \
    -drive format=raw,file=fat:rw:build/esp \
    -drive if=none,id=data,format=raw,file=build/data.img \
    -device virtio-blk-pci,drive=data,disable-legacy=off \
    -serial mon:stdio "$@"
