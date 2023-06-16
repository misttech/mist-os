#!/bin/bash
# Copyright 2020 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -o errexit nounset
set -u
<<<<<<< HEAD

readonly DTS_DIR="$( cd "$(dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
readonly IN_FORMAT="dts"
readonly OUT_FORMAT="dtb"

=======
readonly DTS_DIR="$( cd "$(dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
readonly IN_FORMAT="dts"
readonly OUT_FORMAT="dtb"
>>>>>>> 45c1e0c9dff ([boot-shim] Bootstrap '/chosen')
if ! command -v dtc &>/dev/null; then
  echo "error: dtc is not installed"
  exit 1
fi
<<<<<<< HEAD

=======
>>>>>>> 45c1e0c9dff ([boot-shim] Bootstrap '/chosen')
for source in "${DTS_DIR}"/*."${IN_FORMAT}"
do
  dest="${source%.${IN_FORMAT}}.${OUT_FORMAT}"
  dtc --in-format "${IN_FORMAT}" --out-format "${OUT_FORMAT}" --out "${dest}" "${source}"
done
