#!/bin/bash
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -eu

if [[ $# -ne 3 ]]
then
  echo "Incorrect arguments passed."
  echo "Usage: $0 <dtc path> <dts files> <dtb output path>"
  exit -1
fi

readonly DTC_PATH=$1
readonly DTC_INPUT=$2
readonly DTC_OUTPUT=$3

$DTC_PATH $DTC_INPUT > $DTC_OUTPUT
