#!/bin/bash
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Generates debug symbols directory (_build_id) from a list of ids.txt files.
#
# Usage: generate_symbols_dir.sh <output path> <input files> ...
#
# The input files are a list of single-line ids.txt files in the format of "<build ID> <object_file_with_symbols>"
#
# This script should only be invoked by _fuchsia_package_impl in package.bzl.

# Exits on any failure
set -euf -o pipefail

if [[ $# -lt 2 ]]; then
  echo >&2 "Error: invalid invocation of $0: $@. Expected >= 2 arguments."
  exit 1
fi

readonly output="$1"
shift

for input in "$@"; do
  # Skip empty file, which could mean not ELF file / no build_id info.
  if [[ ! -s "${input}" ]]; then
    continue
  fi

  content=$(<${input})
  # Split by the first space.
  # The content is following the ids.txt format: <build id> <elf_with_symbols>.
  build_id="${content%% *}"
  elf_with_symbols="${content#* }"

  # Convert to build-id directory path,
  # i.e. "de/adbeef.debug" for a "deadbeef" build id.
  build_id_path="${output}/${build_id::2}"
  debug_filename="${build_id:2}.debug"

  # Check whether the source file has symbols and debug_info.
  elf_info="$(file -L "${elf_with_symbols}")"

  # If the file is not stripped, add it to the output no matter whether it has debug_info.
  if [[ "${elf_info}" == *"not stripped"* ]]; then
    dest="${build_id_path}"
    mkdir -p "${build_id_path}"
    cp "${elf_with_symbols}" "${build_id_path}/${debug_filename}"

    # Warn if there's no debug_info.
    if [[ "${elf_info}" != *"with debug_info"* ]]; then
      echo >&2 "WARNING, no debug info in: \"${elf_with_symbols}\""
    fi
  #  else
    # Otherwise, if the file is stripped (not "not stripped"), skip it.
    # echo >&2 "WARNING, binary is already stripped: \"${elf_with_symbols}\""
  fi
done

# Create an empty file if the output directory is empty, as bazel will ignore
# empty directories
if [[ -z "$(ls -A -- "$output")" ]]; then
  touch "${output}"/.ensure_there_is_one_file
fi
