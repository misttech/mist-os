#!/usr/bin/env bash
#
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Produces a super image to test the Android "super" parser library.
#
# REQUIRES:
#   lpmake (https://android.googlesource.com/platform/system/extras/+/master/partition_tools)
#
# PRODUCES:
#   ../test_data/simple_super.img.zstd

rm -f /tmp/simple_super.img ../testdata/simple_super.img.zstd

lpmake --device-size 1073741824 \
  --metadata-size 65536 \
  --metadata-slots 1 \
  --partition system:readonly:4096 \
  --partition system_ext:readonly:0 \
  --block-size 4096 \
  --virtual-ab \
  --force-full-image \
  --output /tmp/simple_super.img

zstd /tmp/simple_super.img -o ../testdata/simple_super.img.zstd
