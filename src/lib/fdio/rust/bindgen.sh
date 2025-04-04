#!/bin/bash

# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
#
# This script generates Rust bindings for fdio.

# Determine paths for this script and its directory, and set $FUCHSIA_DIR.
readonly FULL_PATH="${BASH_SOURCE[0]}"
readonly SCRIPT_DIR="$(cd "$(dirname "${FULL_PATH}")" >/dev/null 2>&1 && pwd)"
source "${SCRIPT_DIR}/../../../../tools/devshell/lib/vars.sh"

set -eu

cd "${SCRIPT_DIR}"

readonly RELPATH="${FULL_PATH#${FUCHSIA_DIR}/}"
readonly BINDGEN="${PREBUILT_RUST_BINDGEN_DIR}/bindgen"

# Generate annotations for the top of the generated source file.
readonly RAW_LINES="// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Generated by ${RELPATH} using $("${BINDGEN}" --version)

#![allow(dead_code)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(unused_results)]
"

readonly OUTPUT="src/fdio_sys.rs"

tmp="$(mktemp --suffix=.h)"
for f in "${FUCHSIA_DIR}"/sdk/lib/fdio/include/lib/fdio/*.h; do
  if [ "${f}" = "${FUCHSIA_DIR}"/sdk/lib/fdio/include/lib/fdio/spawn-actions.h ]; then
    continue
  fi
  # TODO(https://github.com/rust-lang/rust-bindgen/issues/316): Remove this sed
  # invocation when bindgen supports macros containing type casts.
  cat "${f}" | sed -E 's/(#define \w+) \(\(\w+_t\)(\w+)\)/\1 \(\2\)/g' >> "${tmp}"
done

"${BINDGEN}" \
    "${tmp}" \
    --disable-header-comment \
    --raw-line "${RAW_LINES}" \
    --with-derive-default \
    --impl-debug \
    --output "${OUTPUT}" \
    --allowlist-function '(zx|fd)io_.+' \
    --blocklist-function 'fdio_open_.+' \
    --blocklist-function 'fdio_open' \
    --allowlist-type '(zx|fd)io_.+' \
    --allowlist-var 'FDIO_.+' \
    -- \
    -I "${FUCHSIA_DIR}"/sdk/lib/fdio/include \
    -I "${FUCHSIA_DIR}"/zircon/system/public

fx format-code --files=${OUTPUT}
