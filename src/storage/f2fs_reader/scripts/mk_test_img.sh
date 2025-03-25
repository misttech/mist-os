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
dd if=/dev/zero bs=4096 count=65536 of=/tmp/f2fs.img
mkfs.f2fs -f -O encrypt -l testimage /tmp/f2fs.img

# Mount and populate.
MOUNT_PATH=/tmp/f2fs_mnt
mkdir -p ${MOUNT_PATH}
mount -o loop -t f2fs /tmp/f2fs.img ${MOUNT_PATH}

mkdir -p ${MOUNT_PATH}/a/b/c
echo "inline_data" > ${MOUNT_PATH}/a/b/c/inlined
dd if=/dev/zero bs=4096 count=8 of=${MOUNT_PATH}/a/b/c/regular
echo -n "01234567" >> ${MOUNT_PATH}/a/b/c/regular

ln -s regular ${MOUNT_PATH}/a/b/c/symlink
ln ${MOUNT_PATH}/a/b/c/regular ${MOUNT_PATH}/a/b/c/hardlink
touch ${MOUNT_PATH}/a/b/c/chowned
chown 999:999 ${MOUNT_PATH}/a/b/c/chowned

# Large directory (2,000 entries)
mkdir ${MOUNT_PATH}/large_dir
for i in $(seq 0 2000); do
  touch ${MOUNT_PATH}/large_dir/${i}
done

# Large directory with deleted files.
mkdir ${MOUNT_PATH}/large_dir2
for i in $(seq 0 2000); do
  touch ${MOUNT_PATH}/large_dir2/${i}
done
for i in $(seq 0 1999); do
  rm ${MOUNT_PATH}/large_dir2/${i}
done

# Sparse files across nids.
# from i_addrs
echo -n "foo" > ${MOUNT_PATH}/sparse.dat
# from nids[0] n = 923
dd conv=notrunc if=/dev/zero bs=4096 count=1 seek=923 of=${MOUNT_PATH}/sparse.dat
# from nids[1] n += 1018 = 1941
dd conv=notrunc if=/dev/zero bs=4096 count=1 seek=1941 of=${MOUNT_PATH}/sparse.dat
# from nids[2] n += 1018 = 2959
dd conv=notrunc if=/dev/zero bs=4096 count=1 seek=2959 of=${MOUNT_PATH}/sparse.dat
# from nids[3] n += 1018^2 = 1039283
dd conv=notrunc if=/dev/zero bs=4096 count=1 seek=1039283 of=${MOUNT_PATH}/sparse.dat
# from nids[4] n += 1018^2 * 100 = 104671683
dd conv=notrunc if=/dev/zero bs=4096 count=1 seek=104671683 of=${MOUNT_PATH}/sparse.dat
echo -n "bar" >> ${MOUNT_PATH}/sparse.dat

# xattr
attr -s a -V "value" ${MOUNT_PATH}/sparse.dat
attr -s b -V "value" ${MOUNT_PATH}/sparse.dat
attr -s c -V "value" ${MOUNT_PATH}/sparse.dat
attr -r b ${MOUNT_PATH}/sparse.dat


umount ${MOUNT_PATH}
zstd /tmp/f2fs.img -o ../testdata/f2fs.img.zst
