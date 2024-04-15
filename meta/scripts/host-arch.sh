#!/usr/bin/env bash
# Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

readonly HOST_ARCH=$(uname -m)
if [ "$HOST_ARCH" == "aarch64" -o "$HOST_ARCH" == "arm64" ]; then
  readonly ARCH="arm64"
elif [ "$HOST_ARCH" == "x86_64" ]; then
  readonly ARCH="x64"
else
  echo "Arch not supported: $HOST_ARCH"
  exit 1
fi

echo $ARCH
