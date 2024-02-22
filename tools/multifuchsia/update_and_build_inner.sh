#!/bin/bash
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -euE -o pipefail

function swap() {
  local -r file1=$1
  local -r file2=$2
  python3 -c 'import sys; import ctypes; syscall=ctypes.CDLL(None).syscall; syscall.restype=ctypes.c_int; syscall.argtypes=(ctypes.c_long, ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint); sys.exit(syscall(316, -100, sys.argv[1].encode("utf-8"), -100, sys.argv[2].encode("utf-8"), 2))' "$file1" "$file2" || (
    echo "failed to exchange snapshots: $?"
    return 1
  )
}

readonly MOUNTPOINT=$1
readonly MULTIFUCHSIA_WORKSPACE=$2
readonly CLEAN_DIR="$MULTIFUCHSIA_WORKSPACE/clean"

function bind_mount() {
  local -r source="$1"

  if mount --bind "$source" "$MOUNTPOINT" ; then
    # all good
    true
  else
    if findmnt --raw --noheadings --output 'SOURCE' --mountpoint "$MOUNTPOINT" >/dev/null; then
      echo "Working around mounted but broken $MOUNTPOINT" >&2
      local -r mountpoint_parent="$(dirname $MOUNTPOINT)"
      mount -t tmpfs fake_src "$mountpoint_parent"
      mkdir -p "$MOUNTPOINT"
      mount --bind "$source" "$MOUNTPOINT"
    else
      echo "Mount failed but nothing seems to be mounted on $MOUNTPOINT" >&2
      return 1
    fi
  fi
}

bind_mount "$CLEAN_DIR"
(
  cd "$MOUNTPOINT"
  ./.jiri_root/bin/jiri update -gc
  readonly goma_tmp_dir="$(./prebuilt/third_party/goma/linux-x64/gomacc "tmp_dir")"
  mount -t tmpfs isolated_goma "$goma_tmp_dir"
  # goma complains about excessive permissions on this dir
  chmod go-rwx "$goma_tmp_dir"
  # pick alternate ports so we don't interfere with the outside goma
  GOMA_COMPILER_PROXY_PORT=8088 GOMACTL_PROXY_PORT=19081 ./scripts/fx goma_ctl start

  ./scripts/fx build
)

if [ -d "${MULTIFUCHSIA_WORKSPACE}/snapshots/build.new" ]; then
  echo "Deleting stale snapshot build.new" >&2
  btrfs property set "${MULTIFUCHSIA_WORKSPACE}"/snapshots/build.new ro false
  btrfs subvolume delete -c "${MULTIFUCHSIA_WORKSPACE}"/snapshots/build.new
fi
btrfs subvolume snapshot -r "${CLEAN_DIR}" "${MULTIFUCHSIA_WORKSPACE}/snapshots/build.new"
if [ -d "${MULTIFUCHSIA_WORKSPACE}/snapshots/build.success" ]; then
  swap "${MULTIFUCHSIA_WORKSPACE}"/snapshots/build.new "${MULTIFUCHSIA_WORKSPACE}"/snapshots/build.success
  btrfs property set "${MULTIFUCHSIA_WORKSPACE}"/snapshots/build.new ro false
  btrfs subvolume delete -c "${MULTIFUCHSIA_WORKSPACE}"/snapshots/build.new |& grep -v 'WARNING: cannot read default subvolume id: Operation not permitted'
else
  mv "${MULTIFUCHSIA_WORKSPACE}"/snapshots/build.new "${MULTIFUCHSIA_WORKSPACE}"/snapshots/build.success
fi
echo "snapshots/build.success updated" >&2

(
  cd "$MOUNTPOINT"
  goma_stop_output="$(./scripts/fx goma_ctl ensure_stop 2>&1)"
  goma_status="$(./scripts/fx goma_ctl status 2>&1 || true)"
  if echo "$goma_status" | grep -q "goma is not running"; then
    echo "goma stopped." >&2
  else
    echo "failed to stop goma:" >&2
    echo "$goma_status" >&2
    echo "$goma_stop_output" >&2
    exit 1
  fi
)
