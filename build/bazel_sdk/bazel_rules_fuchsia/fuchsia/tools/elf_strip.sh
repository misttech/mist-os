#!/bin/bash
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Strips the binary and produces a single-line ids.txt file
#
# The output file will be a single-line ids.txt file in the format of
# "<elf_with_symbols_file> <debug_file>".
#
# If the source file is not an ELF file or does not contain a build ID, the
# output file will be empty.
#
# This script should only be invoked by fuchsia_package_impl in
# fuchsia_package.bzl.

# Exits on any failure
set -euf -o pipefail

if [[ $# != 4 ]]; then
  echo >&2 "Error: invalid invocation of $0: $@"
  exit 1
fi

readonly _objcopy="$1"
readonly elf_with_symbols_file="$2"
readonly elf_stripped="$3"
readonly ids_txt="$4"

readonly info="$(file -L "${elf_with_symbols_file}")"

# If this file is not an ELF, e.g. a font file or an image,
# or if this file has no symbols to strip, then we just copy this file
# as-is.
if [[ ! "$info" =~ " ELF " || "$info" =~ "no section header" ]]; then
  cp "${elf_with_symbols_file}" "${elf_stripped}"
  # Create an empty ids.txt.
  touch "${ids_txt}"
  exit
fi

# strip symbols from the ELF
"${_objcopy}" --strip-all "${elf_with_symbols_file}" "${elf_stripped}"

# Get build ID.
if ! [[ "$info" =~ (BuildID\[[^\]]*\]=)([^, ]*) ]]; then
  echo >&2 "WARNING: No build id in ELF: ${elf_with_symbols_file}"
  touch "${ids_txt}"
  exit
fi

readonly build_id="${BASH_REMATCH[2]}"
echo "${build_id}" "${elf_with_symbols_file}" > "${ids_txt}"
