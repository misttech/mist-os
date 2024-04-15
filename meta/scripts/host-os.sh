#!/usr/bin/env bash
# Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

HOST_OS=$(uname | tr '[:upper:]' '[:lower:]')
if [[ ${HOST_OS} == "darwin" ]]; then
  HOST_OS="mac"
fi

echo $HOST_OS
