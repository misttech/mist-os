#!/bin/bash
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

BLOCK_SIZE=4k
BLOCK_COUNT=25600

mkdir mnt

# Simple IO test
IMAGE_NAME=simple_io.img
dd if=/dev/zero of=${IMAGE_NAME} bs=${BLOCK_SIZE} count=${BLOCK_COUNT}
mkfs.f2fs -f ${IMAGE_NAME}
mount ${IMAGE_NAME} mnt
cd mnt
echo "hello" > test
cd ..
umount mnt
zstd ${IMAGE_NAME}
rm ${IMAGE_NAME}

rmdir mnt
