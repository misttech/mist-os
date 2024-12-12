#! /bin/sh
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script prints "true" if the file exists, else "false".

file="$1"
if test -f "$file"; then
  echo "true"
else
  echo "false"
fi
