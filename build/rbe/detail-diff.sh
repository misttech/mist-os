#!/bin/bash
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# A diff wrapper that analyzes differences with a variety of tools.
# Exit status of this script is not meaningful.

script="$0"
script_dir="$(dirname "$script")"

source "$script_dir"/common-setup.sh

project_root="$(realpath "$script_dir"/../..)"
project_root_rel="$(relpath . "$project_root")"

diff_limit=25
clang_dir_local="$project_root_rel"/prebuilt/third_party/clang/"$HOST_PLATFORM"

# Tools
objdump="$clang_dir_local"/bin/llvm-objdump
readelf="$clang_dir_local"/bin/llvm-readelf
dwarfdump="$clang_dir_local"/bin/llvm-dwarfdump


# Diff two files, run through a command: diff -u <(command $1) <(command $2)
# Usage: diff_with command [options] -- input1 input2
function diff_with() {
  local tool
  local inputs
  tool=()
  for token in "$@"
  do
    case "$token" in
      --) shift; break ;;
      *) tool+=("$token") ;;
    esac
    shift
  done

  # The rest of "$@" are input files.  $1 is from local, $2 is from remote.
  test "$#" = 2 || {
    echo "diff_with: Expected two inputs, but got $#."
    exit 1
  }

  # Some tools' output include the full name of the file
  # being examined, and behavior may depend on the file extension.
  # So use $1 as the canonical name for tool operation on both files.
  suffix="$(basename "${tool[0]}")"

  "${tool[@]}" "$1" > "$1.$suffix.local"
  # Use the same name for the remote file with a temporary move.
  mv "$1"{,.bkp}
  mv "$2" "$1"
  "${tool[@]}" "$1" > "$1.$suffix.remote"
  # Restore the original names.
  mv "$1" "$2"
  mv "$1"{.bkp,}

  echo "diff -u <(${tool[@]} $1) <(${tool[@]} $2)"
  diff -u "$1.$suffix.local" "$1.$suffix.remote"
}

function binary_diff() {
  # Intended for binaries (rlibs, executables).
  echo "objdump-diff (first $diff_limit lines):"
  diff_with "$objdump" --full-contents -- "$1" "$2" | head -n "$diff_limit"
  echo

  echo "readelf-diff (first $diff_limit lines):"
  diff_with "$readelf" -a -- "$1" "$2" | head -n "$diff_limit"
  echo

  echo "dwarfdump-diff (first $diff_limit lines):"
  diff_with "$dwarfdump" -a -- "$1" "$2" | head -n "$diff_limit"
  echo

  echo "nm-diff (first $diff_limit lines):"
  diff_with nm -- "$1" "$2" | head -n "$diff_limit"
  echo

  echo "strings-diff (first $diff_limit lines):"
  diff_with strings -- "$1" "$2" | head -n "$diff_limit"
}

case "$1" in
  *.d | *.map | *.ll)
    echo "text diff (first $diff_limit lines):"
    diff -u "$1" "$2" | head -n "$diff_limit"
    ;;
  # TODO: .bc LLVM bitcode
  *.a | *.o | *.so | *.rlib)
    binary_diff "$1" "$2"
    ;;
  *)
    filetype="$(file "$1" | head -n 1 | sed -e "s|^$1: ||")"
    case "$filetype" in
      *executable* | *"shared object"* | *"ar archive"* | *ELF*relocatable* )
        binary_diff "$1" "$2"
        ;;
      *)
        # Unknown type, default to text.
        diff -u "$1" "$2" | head -n "$diff_limit"
    esac
    ;;
esac

exit 0
