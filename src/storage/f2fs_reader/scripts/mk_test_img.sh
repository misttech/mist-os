#!/bin/bash
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
#
# Produces a small test image for use with f2fs_reader.
#
# Requires root. Produces ../testdata/f2fs.img.zst
# Usage:
#   # sudo ${PWD}/mk_test_img.sh
set -e

# Prerequisites.
apt-get install f2fs-tools zstd
modprobe f2fs
rm -f /tmp/f2fs.img ../testdata/f2fs.img.zst

# Build empty image.
dd if=/dev/zero bs=4096 count=16384 of=/tmp/f2fs.img
mkfs.f2fs -f -O encrypt -l testimage /tmp/f2fs.img

# Mount and populate.
MOUNT_PATH=/tmp/f2fs_mnt
mkdir -p ${MOUNT_PATH}
mount -o loop -t f2fs /tmp/f2fs.img ${MOUNT_PATH}

mkdir -p ${MOUNT_PATH}/a/b/c
echo "inline_data" > ${MOUNT_PATH}/a/b/c/inlined
dd if=/dev/zero bs=4096 count=8 of=${MOUNT_PATH}/a/b/c/regular
ln -s regular ${MOUNT_PATH}/a/b/c/symlink
ln ${MOUNT_PATH}/a/b/c/regular ${MOUNT_PATH}/a/b/c/hardlink
touch ${MOUNT_PATH}/a/b/c/chowned
chown nobody:nobody ${MOUNT_PATH}/a/b/c/chowned

umount ${MOUNT_PATH}
zstd /tmp/f2fs.img -o ../testdata/f2fs.img.zst
