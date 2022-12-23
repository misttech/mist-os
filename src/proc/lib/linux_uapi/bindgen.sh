#!/bin/sh
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -e

if [[ ! -f src/proc/lib/linux_uapi/wrapper.h ]]; then
  echo 'Please run this script from the root of your Fuchsia source tree.'
  exit 1
fi

readonly RAW_LINES="// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_snake_case)]

use zerocopy::{AsBytes, FromBytes};

pub use crate::x86_64_types::*;

unsafe impl<Storage> AsBytes for __BindgenBitfieldUnit<Storage>
where
    Storage: AsBytes,
{
    fn only_derive_is_allowed_to_implement_this_trait() {}
}

unsafe impl<Storage> FromBytes for __BindgenBitfieldUnit<Storage>
where
    Storage: FromBytes,
{
    fn only_derive_is_allowed_to_implement_this_trait() {}
}"

PATH="$PWD/prebuilt/third_party/rust/linux-x64/bin:$PATH" \
./prebuilt/third_party/rust_bindgen/linux-x64/bindgen \
  --no-layout-tests \
  --size_t-is-usize \
  --ignore-functions \
  --with-derive-default \
  --explicit-padding \
  --opaque-type=__sighandler_t \
  --opaque-type=__sigrestore_t \
  --opaque-type=group_filter.* \
  --opaque-type=sigevent \
  --ctypes-prefix="crate::x86_64_types" \
  --raw-line "${RAW_LINES}" \
  -o src/proc/lib/linux_uapi/src/x86_64.rs \
  src/proc/lib/linux_uapi/wrapper.h \
  -- \
  -target x86_64-pc-linux-gnu \
  -I third_party/android/platform/bionic/libc/kernel/uapi \
  -I third_party/android/platform/bionic/libc/kernel/uapi/asm-x86 \
  -I third_party/android/platform/bionic/libc/kernel/android/uapi \
  -I src/proc/lib/linux_uapi/stub \
  -nostdlibinc

# TODO(https://github.com/rust-lang/rust-bindgen/issues/2170): Remove in favor of bindgen support
# for custom derives.
sed -i \
  's/derive(Debug, Default, Copy, Clone)/derive(Debug, Default, Copy, Clone, AsBytes, FromBytes)/' \
  src/proc/lib/linux_uapi/src/x86_64.rs

# Adds a derive for `FromBytes` for the given type.
#
# Params:
#   $1: The name of the type.
function auto_derive_from_bytes_for() {
  # If the first line matches the expected derive expression, consume another line and do a search
  # and replace for the expected identifier, rewriting the derive expression to include `FromBytes`.
  # TODO(https://github.com/rust-lang/rust-bindgen/issues/2170): Remove in favor of bindgen support
  # for custom derives.
  sed -i \
    "/#\[derive(.*)\]/ { N; /FromBytes/! s/#\[derive(\(.*\))\]\n\(pub \(struct\|union\) $1\)/#[derive(\1, FromBytes)]\n\2/; p; d; }" \
    src/proc/lib/linux_uapi/src/x86_64.rs

  # Use CStr to represent constant C strings.
  sed -i 's/: &\[u8; [0-9][0-9]*usize\] = \(b".*\)\\0";$/: '"\&'"'static std::ffi::CStr = unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(\1\\0") };/g' \
    src/proc/lib/linux_uapi/src/x86_64.rs
}

auto_derive_from_bytes_for binder_transaction_data
auto_derive_from_bytes_for flat_binder_object
auto_derive_from_bytes_for bpf_attr
auto_derive_from_bytes_for __IncompleteArrayField
auto_derive_from_bytes_for ipt_get_entries
auto_derive_from_bytes_for ipt_replace
auto_derive_from_bytes_for ipt_entry
auto_derive_from_bytes_for ip6t_ip6
auto_derive_from_bytes_for in6_addr
auto_derive_from_bytes_for xt_counters_info

# Adds a derive for `FromBytes` for the given type.
#
# Use this function instead of `auto_derive_from_bytes_for` when the type doesn't have any derives.
#
# Params:
#   $1: The name of the type.
function add_derive_from_bytes_for() {
  sed -i \
    "/pub \(struct\|union\) $1/i #[derive(FromBytes)]" \
    src/proc/lib/linux_uapi/src/x86_64.rs
}

add_derive_from_bytes_for ip6t_get_entries
add_derive_from_bytes_for ip6t_entry
add_derive_from_bytes_for ip6t_replace

function replace_raw_pointer_with_user_address() {
  sed -i "s/*\(const\|mut\) $1/u64/" src/proc/lib/linux_uapi/src/x86_64.rs
}

replace_raw_pointer_with_user_address xt_counters

scripts/fx format-code --files=src/proc/lib/linux_uapi/src/x86_64.rs
