#!/bin/sh
# Builds the kernel and puts it where a UEFI firmware looks for it:
# build/esp/EFI/BOOT/BOOTX64.EFI. Copy build/esp to a FAT32 USB stick to
# start a real machine.
#
# Uses the local cargo when it has the UEFI target (rustup), and otherwise
# the Docker image "xilaos-build" (rust:1-slim with the target added).
set -e
cd "$(dirname "$0")/.."

if rustup target list --installed 2>/dev/null | grep -q x86_64-unknown-uefi; then
    cargo build --release -p xila-kernel --target x86_64-unknown-uefi --target-dir target/uefi
    out=target/uefi
else
    if ! docker image inspect xilaos-build >/dev/null 2>&1; then
        printf 'FROM rust:1-slim\nRUN rustup target add x86_64-unknown-uefi\n' | docker build -t xilaos-build -
    fi
    sync
    # the outside libraries (xila-tls) are fetched once and kept between builds
    mkdir -p target/docker-registry
    docker run --rm -v "$PWD":/x -v "$PWD/target/docker-registry":/usr/local/cargo/registry -w /x xilaos-build \
        cargo build --release -p xila-kernel --target x86_64-unknown-uefi --target-dir target/docker
    out=target/docker
fi

mkdir -p build/esp/EFI/BOOT
cp "$out/x86_64-unknown-uefi/release/xila-kernel.efi" build/esp/EFI/BOOT/BOOTX64.EFI
echo "build/esp/EFI/BOOT/BOOTX64.EFI"
