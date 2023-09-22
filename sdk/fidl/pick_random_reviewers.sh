#!/usr/bin/env bash
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script returns a random set of API reviewers from the owners file.

if [ -z "$1" ]; then
  COUNT="3"
else
  COUNT=$1
fi

SCRIPT_SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"

# Omit comment lines, omit empty lines, choose $COUNT out of the set of
# unique API reviewers.
cat ${SCRIPT_SRC_DIR}/OWNERS | grep -v '#' | grep -v '^$' | sort | uniq | shuf | head -n "${COUNT}"
