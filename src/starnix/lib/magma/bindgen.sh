#!/bin/sh
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -e

if [[ ! -f sdk/lib/magma_client/include/lib/magma/magma.h ]]; then
  echo 'Please run this script from the root of your Fuchsia source tree.'
  exit 1
fi

readonly RAW_LINES="// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use zerocopy::{AsBytes, FromBytes};"

# Type/define pairs, used to generate a list of variables to work around
# https://github.com/rust-lang/rust-bindgen/issues/316
readonly define_list=(
magma_status_t,MAGMA_STATUS_OK
magma_status_t,MAGMA_STATUS_INVALID_ARGS
uint32_t,MAGMA_IMAGE_CREATE_FLAGS_PRESENTABLE
uint32_t,MAGMA_IMAGE_CREATE_FLAGS_VULKAN_USAGE
uint32_t,MAGMA_MAX_IMAGE_PLANES
magma_coherency_domain_t,MAGMA_COHERENCY_DOMAIN_CPU
magma_coherency_domain_t,MAGMA_COHERENCY_DOMAIN_RAM
magma_coherency_domain_t,MAGMA_COHERENCY_DOMAIN_INACCESSIBLE
uint32_t,MAGMA_POLL_TYPE_SEMAPHORE
uint32_t,MAGMA_POLL_TYPE_HANDLE
magma_cache_policy_t,MAGMA_CACHE_POLICY_CACHED
magma_cache_policy_t,MAGMA_CACHE_POLICY_WRITE_COMBINING
magma_cache_policy_t,MAGMA_CACHE_POLICY_UNCACHED
magma_query_t,MAGMA_QUERY_VENDOR_ID
)

define_text=""
for define in ${define_list[@]}; do
  TYPE=${define%,*};
  NAME=${define#*,};

  # Create a variable with the same name as the define, so bindgen can use its value.
  define_text+="
const $TYPE _$NAME = $NAME;
#undef $NAME
const $TYPE $NAME = _$NAME;"
done

temp_include_dir=$(mktemp -d)

function cleanup {
  rm -rf "$temp_include_dir"
}

trap cleanup EXIT

echo "$define_text" > $temp_include_dir/missing_includes.h
PATH="$PWD/prebuilt/third_party/rust/linux-x64/bin:$PATH" \
./prebuilt/third_party/rust_bindgen/linux-x64/bindgen \
  --no-layout-tests \
  --with-derive-default \
  --explicit-padding \
  --raw-line "${RAW_LINES}" \
  -o src/starnix/lib/magma/src/magma.rs \
  src/starnix/lib/magma/wrapper.h \
  -- \
  -I zircon/system/public \
  -I $(scripts/fx get-build-dir)/linux_x64/gen/src/graphics/lib/magma/include \
  -I src/graphics/lib/magma/src \
  -I src/graphics/lib/magma/include \
  -I sdk/lib/magma_client/include \
  -I $temp_include_dir \
  -I $(pwd)

# TODO: Figure out how to get bindgen to derive AsBytes and FromBytes.
#       See https://github.com/rust-lang/rust-bindgen/issues/1089
sed -i \
  's/derive(Debug, Default, Copy, Clone)/derive(Debug, Default, Copy, Clone, AsBytes, FromBytes)/' \
  src/starnix/lib/magma/src/magma.rs
