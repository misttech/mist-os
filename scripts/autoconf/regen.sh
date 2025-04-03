#!/bin/bash
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# A script to regenerate config.h files for Fuchsia.

source "$FUCHSIA_DIR"/tools/devshell/lib/vars.sh

set -euxo pipefail

FXSET_WITH_ADDITIONAL=""
FXBUILD_WITH_ADDITIONAL=""
CPPFLAGS_ADDITIONAL=""
LDFLAGS_ADDITIONAL=""
LINUX_LIBRARY=""

for ARGUMENT in "$@"
do
    KEY=$(echo "$ARGUMENT" | cut -f1 -d=)
    VALUE=$(echo "$ARGUMENT" | cut -f2- -d=)

    case "$KEY" in
      FUCHSIA_OUT_CONFIG_H) FUCHSIA_OUT_CONFIG_H="$VALUE" ;;
      LINUX_OUT_CONFIG_H) LINUX_OUT_CONFIG_H="$VALUE" ;;
      FXSET_WITH_ADDITIONAL) FXSET_WITH_ADDITIONAL="$VALUE" ;;
      FXBUILD_WITH_ADDITIONAL) FXBUILD_WITH_ADDITIONAL="$VALUE" ;;
      CPPFLAGS_ADDITIONAL) CPPFLAGS_ADDITIONAL="$VALUE" ;;
      LDFLAGS_ADDITIONAL) LDFLAGS_ADDITIONAL="$VALUE" ;;
      LINUX_LIBRARY) LINUX_LIBRARY="$VALUE" ;;
      REPO_ZIP_URL) REPO_URL="$VALUE" ;;
      REPO_EXTRACTED_FOLDER) REPO_EXTRACTED_FOLDER="$VALUE" ;;
      *)
        cat <<EOF
Variables:

  FUCHSIA_OUT_CONFIG_H        Path to the generated config.h for fuchsia (required)
  LINUX_OUT_CONFIG_H          Path to the generated config.h for linux (required)
  FXSET_WITH_ADDITIONAL       Additional args for fx set
  FXBUILD_WITH_ADDITIONAL     Additional args for fx build
  CPPFLAGS_ADDITIONAL         Addtional CPP flags (passed to the configure script)
  LDFLAGS_ADDITIONAL          Additional LD flags
  LINUX_LIBRARY               Path to library to link with on linux host
  REPO_ZIP_URL                The URL for the upstream repo (required)
  REPO_EXTRACTED_FOLDER       The folder that the repo unzips to (required)
EOF
        exit 1
    esac
done

fx set core.x64 --auto-dir $FXSET_WITH_ADDITIONAL
fx build sdk:zircon_sysroot $FXBUILD_WITH_ADDITIONAL

readonly BUILD_DIR="$FUCHSIA_OUT_DIR/core.x64"
readonly FUCHSIA_SYSROOT_DIR="$BUILD_DIR/sdk/exported/zircon_sysroot/arch/x64/sysroot"
readonly LINUX_SYSROOT_DIR="$FUCHSIA_DIR/prebuilt/third_party/sysroot/linux"
readonly TARGET=x86_64-unknown-fuchsia

TMP_REPO="$(mktemp -d)"
readonly TMP_REPO
cleanup() {
  rm -rf "$TMP_REPO"
}
trap cleanup EXIT

wget -O "$TMP_REPO/repo.zip" "$REPO_URL"
unzip "$TMP_REPO/repo.zip" -d "$TMP_REPO"
readonly TMP_REPO_EXTRACTED="$TMP_REPO/$REPO_EXTRACTED_FOLDER"

readonly CC="$PREBUILT_CLANG_DIR/bin/clang"
CC_INCLUDE=$("$CC" --print-file-name=include)
readonly CC_INCLUDE

autoreconf "$TMP_REPO_EXTRACTED"
cd "$TMP_REPO_EXTRACTED"
./configure \
  --host $TARGET \
  CC="$CC" \
  CFLAGS="-target $TARGET -nostdinc -nostdlib" \
  CPPFLAGS="-I$FUCHSIA_SYSROOT_DIR/include -I$CC_INCLUDE $CPPFLAGS_ADDITIONAL" \
  LDFLAGS="-L$FUCHSIA_SYSROOT_DIR/lib -L$BUILD_DIR/x64-shared -lc $LDFLAGS_ADDITIONAL"

COPYRIGHT="// Copyright $(date +"%Y") The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
"

cat <(echo "$COPYRIGHT") "$TMP_REPO_EXTRACTED/config.h" > "$FUCHSIA_OUT_CONFIG_H"

LDFLAGS="$LDFLAGS_ADDITIONAL"
if [ "$LINUX_LIBRARY" ]
then
  LDFLAGS="-L$BUILD_DIR/host_x64/obj/$LINUX_LIBRARY $LDFLAGS"
fi
./configure \
  CC="$CC" \
  CFLAGS="--sysroot=$LINUX_SYSROOT_DIR" \
  CPPFLAGS="$CPPFLAGS_ADDITIONAL" \
  LDFLAGS="$LDFLAGS"

cat <(echo "$COPYRIGHT") "$TMP_REPO_EXTRACTED/config.h" > "$LINUX_OUT_CONFIG_H"
