#!/bin/bash
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -eu
readonly DTC_PATH=$1
shift
readonly INCLUDE_DIRS=$1
shift

DTC_ARGS=""
if [[ "$INCLUDE_DIRS" != "--" ]]; then
  for i in `cat "$INCLUDE_DIRS"`; do
    DTC_ARGS="$DTC_ARGS -i $i"
  done
fi

set +e
DTC_OUTPUT=$($DTC_PATH $@ $DTC_ARGS 2>&1)
DTC_ERROR_CODE=$?
set -e
echo -en "$DTC_OUTPUT"

# Treat DTS warnings as build failures.
if [[ "$DTC_OUTPUT" == *"Warning"* ]]; then
  echo "$0: Returning failure due to warnings in dts compilation."
  exit 1
fi

exit $DTC_ERROR_CODE
