#!/bin/bash

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -e

mkdir -p "${1}"
echo '#define UBPF_HAS_ELF_H 1' > "${1}/ubpf_config.h"
