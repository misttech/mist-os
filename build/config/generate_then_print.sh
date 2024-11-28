#!/bin/sh
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Run a generator script, with arguments, collect its output, write it
# to a file before sending it to stdout.
#
# Usage:
#   <this_script> OUTPUT_FILE GENERATOR_SCRIPT [GENERATOR_ARGS...]
#
# Where GENERATOR_ARGS may also contain OUTPUT_FILE.

set -e

die () {
  echo >&2 "ERROR: $*"
  return 1
}

case $# in
  0) die "Missing output file (1st argument)";;
  1) die "Missing generator script path (2nd argument)";;
  *);;
esac

output_file="$1"
shift

temp_output_file="${output_file}.tmp"
"$@" > "${temp_output_file}" || die "Generator script returned error $?!"


update_file=
if ! test -f "${output_file}"; then
  update_file=true
elif ! cmp --quiet "${temp_output_file}" "${output_file}"; then
  update_file=true
fi
if test -n "${update_file}"; then
  mv -f "${temp_output_file}" "${output_file}"
fi

cat "${output_file}"
