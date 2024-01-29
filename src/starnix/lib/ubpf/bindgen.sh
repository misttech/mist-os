#!/bin/sh
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -e

if [[ ! -f src/starnix/lib/ubpf/wrapper.h ]]; then
  echo 'Please run this script from the root of your Fuchsia source tree.'
  exit 1
fi

readonly RAW_LINES="// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(non_upper_case_globals)]

"

PATH="$PWD/prebuilt/third_party/rust/linux-x64/bin:$PATH" \
./prebuilt/third_party/rust_bindgen/linux-x64/bindgen \
  --no-layout-tests \
  --with-derive-default \
  --explicit-padding \
  --allowlist-function "ubpf_.*" \
  --allowlist-item "EBPF_.*" \
  --raw-line "${RAW_LINES}" \
  -o src/starnix/lib/ubpf/src/ubpf.rs \
  src/starnix/lib/ubpf/wrapper.h \
  -- \
  -I . \
  -I third_party/android/platform/bionic/libc/include/ \
  -I third_party/ubpf/src/vm/inc \
  -I out/default/gen/third_party/ubpf
