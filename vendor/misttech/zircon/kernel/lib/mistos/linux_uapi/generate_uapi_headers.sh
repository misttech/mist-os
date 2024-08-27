#!/bin/bash
#
# Copyright (C) 2024 Mist Tecnologia LTDA
# Copyright (C) 2013 The Android Open Source Project
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

###  Usage: generate_uapi_headers.sh [<options>]
###
###  This script is used to get a copy of the uapi kernel headers
###  from an android kernel tree and copies them into an android source
###  tree without any processing. The script also creates all of the
###  generated headers and copies them into the android source tree.
###
###  Options:
###   --download-kernel
###     Automatically create a temporary git repository and check out the
###     linux kernel source code at TOT.
###   --use-kernel-dir <DIR>
###     Do not check out the kernel source, use the kernel directory
###     pointed to by <DIR>.
###   --branch <NAME | TAG>
###     Kernel branch to download (branch name or tag, e.g.:v6.6)
###   --target-dir <DIR>
###     Relative path from the current <DIR> to store the kernel generated headers


# Terminate the script if any command fails.
set -eE

TMPDIR=""
KERNEL_DIR=""
KERNEL_REPO="https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git"
KERNEL_BRANCH="master"
ARCH_LIST=("x86")
TARGET_DIR=

function cleanup () {
  if [[ "${TMPDIR}" =~ ${TARGET_DIR} ]] && [[ -d "${TMPDIR}" ]]; then
    echo "Removing temporary directory ${TMPDIR}"
    rm -rf "${TMPDIR}"
    TMPDIR=""
  fi
}

function usage () {
  grep '^###' $0 | sed -e 's/^###//'
}

trap cleanup EXIT
# This automatically triggers a call to cleanup.
trap "exit 1" HUP INT TERM TSTP

while [ $# -gt 0 ]; do
  case "$1" in
    "--download-kernel")
      KERNEL_DOWNLOAD=1
      ;;
    "--use-kernel-dir")
      if [[ $# -lt 2 ]]; then
        echo "--use-kernel-dir requires an argument."
        exit 1
      fi
      shift
      KERNEL_DIR="$1"
      ;;
    "--out-dir")
      if [[ $# -lt 2 ]]; then
        echo "--use-kernel-dir requires an argument."
        exit 1
      fi
      shift
      TARGET_DIR="$1"
      ;;  
    "--branch")
      if [[ $# -lt 2 ]]; then
        echo "--branch requires an argument."
        exit 1
      fi
      shift
      KERNEL_BRANCH="$1"
      ;;  
    "-h" | "--help")
      usage
      exit 1
      ;;
    "-"*)
      echo "Error: Unrecognized option $1"
      usage
      exit 1
      ;;
    *)
      echo "Error: Extra arguments on the command-line."
      usage
      exit 1
      ;;
  esac
  shift
done

TARGET_DIR=$PWD/$TARGET_DIR

if [[ ${KERNEL_DOWNLOAD} -eq 1 ]]; then
  TMPDIR=$(mktemp -d ${TARGET_DIR}/kernelXXXXXXXX)
  cd "${TMPDIR}"
  echo "Fetching linux kernel source..."
  git clone ${KERNEL_REPO} -b ${KERNEL_BRANCH} --depth=1
  cd linux
  KERNEL_DIR="${TMPDIR}/linux"
elif [[ "${KERNEL_DIR}" == "" ]]; then
  echo "Must specify one of --use-kernel-dir or --download-kernel."
  exit 1
elif [[ ! -d "${KERNEL_DIR}" ]] || [[ ! -d "${KERNEL_DIR}/kernel" ]]; then
  echo "The kernel directory $KERNEL_DIR or $KERNEL_DIR/linux does not exist."
  exit 1
else
  cd "${KERNEL_DIR}"
fi

# Completely delete the old original headers
rm -rf "${TARGET_DIR}/uapi"
mkdir -p "${TARGET_DIR}/uapi"

# Build all of the generated headers.
for arch in "${ARCH_LIST[@]}"; do
  echo "Generating headers for arch ${arch}"
  # Clean up any leftover headers.
  make ARCH=${arch} distclean
  make ARCH=${arch} INSTALL_HDR_PATH=${TARGET_DIR}/uapi/${arch} headers_install
done

# Clean all of the generated headers.
for arch in "${ARCH_LIST[@]}"; do
  echo "Cleaning kernel files for arch ${arch}"
  make ARCH=${arch} -C ${KERNEL_DIR} distclean
done

