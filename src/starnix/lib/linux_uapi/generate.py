#!/usr/bin/env -S python3 -B
# allow-non-vendored-python
#
# TODO(b/295039695): We intentionally use the host python3 here instead of
# fuchsia-vendored-python. This script calls out to cbindgen that is not part
# of the Fuchsia repo and must be installed on the local host.
#
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os, sys

sys.path.append(os.path.dirname(os.path.abspath(__file__)) + "/..")
from bindgen import Bindgen

RAW_LINES = """
use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};

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
}

unsafe impl<Storage> FromZeros for __BindgenBitfieldUnit<Storage>
where
    Storage: FromZeros,
{
    fn only_derive_is_allowed_to_implement_this_trait() {}
}

#[repr(transparent)]
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Hash, Ord, PartialOrd, AsBytes, FromBytes, FromZeros, NoCell)]
pub struct uaddr {
    pub addr: u64,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Hash, Ord, PartialOrd, AsBytes, FromBytes, FromZeros, NoCell)]
#[repr(transparent)]
pub struct uref<T> {
    pub addr: uaddr,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> From<uaddr> for uref<T> {
    fn from(addr: uaddr) -> Self {
        Self { addr, _phantom: Default::default() }
    }
}
"""

# Tell bindgen not to produce records for these types.
OPAQUE_TYPES = [
    "group_filter.*",
    "sigval",
    "StdAtomic.*",
]

# Cross-architecture include paths (the ArchInfo class also has an arch-specific one to add).
INCLUDE_DIRS = [
    "third_party/android/platform/bionic/libc/kernel/uapi",
    "third_party/android/platform/bionic/libc/kernel/android/uapi",
    "src/starnix/lib/linux_uapi/stub",
]

# Additional traits that should be added to types matching the regexps.
AUTO_DERIVE_TRAITS = [
    (r"__BindgenBitfieldUnit", ["NoCell"]),
    (
        r"__IncompleteArrayField",
        ["Clone", "AsBytes, FromBytes", "NoCell", "FromZeros"],
    ),
    (r"__BindgenUnionField", ["AsBytes, FromBytes", "NoCell", "FromZeros"]),
    (
        r"__sifields__bindgen_ty_(2|3|7)",
        ["AsBytes, FromBytes", "NoCell", "FromZeros"],
    ),
    (
        r"binder_transaction_data__bindgen_ty_2__bindgen_ty_1",
        ["AsBytes", "FromBytes", "NoCell", "FromZeros"],
    ),
    (r"binder_transaction_data.*", ["FromBytes", "NoCell", "FromZeros"]),
    (
        r"bpf_attr__bindgen_ty_(1|3|5|6|7|9|10|11|12|13|16|17|18|19)(_.+)?$",
        ["AsBytes", "FromBytes", "NoCell", "FromZeros"],
    ),
    (r"bpf_attr.*", ["FromBytes", "NoCell", "FromZeros"]),
    (r"flat_binder_object.*", ["FromBytes", "NoCell", "FromZeros"]),
    (
        r"usb_functionfs_event.*",
        ["Clone", "AsBytes", "FromBytes", "NoCell", "FromZeros"],
    ),
    (r"fuse_dirent", ["Clone", "AsBytes", "FromBytes", "NoCell", "FromZeros"]),
    (r"ifreq.*", ["FromBytes", "NoCell", "FromZeros"]),
    (r"if_settings.*", ["FromBytes", "NoCell", "FromZeros"]),
    (r"in6_addr", ["AsBytes", "FromBytes", "NoCell", "FromZeros"]),
    (r"in6_pktinfo", ["AsBytes", "FromBytes", "NoCell", "FromZeros"]),
    (r"inotify_event", ["AsBytes", "NoCell"]),
    (r"dm_name_list", ["AsBytes", "NoCell", "FromBytes", "FromZeros"]),
    (r"dm_target_versions", ["AsBytes", "NoCell", "FromBytes", "FromZeros"]),
    (
        r"fsverity_digest",
        ["Clone", "AsBytes", "NoCell", "FromBytes", "FromZeros"],
    ),
    (
        r"input_event",
        ["AsBytes", "FromBytes", "NoCell", "FromZeros", "PartialEq"],
    ),
    (r"input_id", ["AsBytes", "FromBytes", "NoCell", "FromZeros", "PartialEq"]),
    (r"ip6t_ip6", ["AsBytes", "FromBytes", "NoCell", "FromZeros"]),
    (r"ip6?t_entry", ["AsBytes", "FromBytes", "NoCell", "FromZeros"]),
    (r"ip6?t_get_entries", ["FromBytes", "NoCell", "FromZeros"]),
    (r"ip6?t_replace", ["AsBytes", "FromBytes", "NoCell", "FromZeros"]),
    (r"robust_list_head", ["FromBytes", "NoCell", "FromZeros"]),
    (r"robust_list", ["FromBytes", "NoCell", "FromZeros"]),
    (r"sigevent", ["FromBytes", "NoCell", "FromZeros"]),
    (r"sigval", ["AsBytes", "FromBytes", "NoCell", "FromZeros"]),
    (r"__sk_buff", ["AsBytes", "FromBytes", "NoCell", "FromZeros"]),
    (r"bpf_sock_addr", ["AsBytes", "FromBytes", "NoCell", "FromZeros"]),
    (r"sockaddr_in*", ["AsBytes", "FromBytes", "NoCell", "FromZeros"]),
    (r"sockaddr_ll*", ["AsBytes", "FromBytes", "NoCell", "FromZeros"]),
    (r"sock_fprog", ["FromBytes", "NoCell", "FromZeros"]),
    (r"sysinfo", ["AsBytes", "NoCell"]),
    (r"timeval", ["AsBytes", "FromBytes", "NoCell", "FromZeros", "PartialEq"]),
    (r"xt_counters_info", ["FromBytes", "NoCell", "FromZeros"]),
]

# General replacements to apply to the contents of the file. These are tuples of
# compiled regular expressions + the thing to replace matches with.
REPLACEMENTS = [
    # Use CStr to represent constant C strings. The inputs look like:
    #   pub const FS_KEY_DESC_PREFIX: &[u8; 9usize] = b"fscrypt:\0";
    (
        r': &\[u8; [0-9]+(usize)?\] = b"(.*)\\0";\n',
        ': &\'static std::ffi::CStr = c"\\2";\n',
    ),
    # Change `__IncompleteArrayField` representation to `transparent`, which is necessary to
    # allow it to derive `AsBytes`.
    # TODO(https://github.com/google/zerocopy/issues/10): Remove this once zerocopy is updated
    # to allow `AsBytes` for generic structs with `repr(C)`.
    (
        r"#\[repr\(C\)\]\n"
        r"#\[derive\((([A-Za-z]+, )*[A-Za-z]+)\)\]\n"
        r"pub struct (__IncompleteArrayField|__BindgenUnionField)",
        """#[repr(transparent)]
        #[derive(\\1)]
        pub struct \\3""",
    ),
    # Add AsBytes/FromBytes/FromZeros to every copyable struct regardless of
    # name.
    # TODO(https://github.com/rust-lang/rust-bindgen/issues/2170):
    # Remove in favor of bindgen support for custom derives.
    (
        r"\n#\[derive\(Debug, Default, Copy, Clone(, FromBytes)?\)\]\n",
        "\n#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes, NoCell, FromZeros)]\n",
    ),
    (
        r"\n#\[derive\(Debug, Copy, Clone(, FromBytes)?\)\]\n",
        "\n#[derive(Debug, Copy, Clone, AsBytes, FromBytes, NoCell, FromZeros)]\n",
    ),
    # Use uaddr/uref in place of pointers for compat with zerocopy traits. Because
    # the target of the pointer is in userspace anyway, treating it as an opaque
    # pointer is harmless.
    (r"\*mut crate::types::c_void", "uaddr"),
    (
        r'::std::option::Option<unsafe extern "C" fn\([a-zA-Z_0-9: ]*\)>',
        "uaddr",
    ),
    (r"([:=]) \*(const|mut) ([a-zA-Z_0-9:]*)", "\\1 uref<\\3>"),
    # Convert atomic wrapper.
    (r": StdAtomic([UI])(8|16|32|64)", ": std::sync::atomic::Atomic\\1\\2"),
    # Remove __bindgen_missing from the start of constants defined in missing_includes.h
    (r"const __bindgen_missing_([a-zA-Z_0-9]+)", "const \\1"),
]

INPUT_FILE = "src/starnix/lib/linux_uapi/wrapper.h"

NO_DEBUG_TYPES = [
    "__sifields__bindgen_ty_(2|3)",
]

NO_COPY_TYPES = [
    "StdAtomic.*",
]


class ArchInfo:
    def __init__(self, name, clang_target, include):
        self.name = name  # Our internal arch name.
        self.clang_target = clang_target  # Clang "triple" name for this arch.
        self.include = (
            include  # Include directory for the arch-specific uapi files.
        )


ARCH_INFO = [
    ArchInfo(
        "x86_64",
        "x86_64-pc-linux-gnu",
        "third_party/android/platform/bionic/libc/kernel/uapi/asm-x86",
    ),
    ArchInfo(
        "arm64",
        "aarch64-linux-gnu",
        "third_party/android/platform/bionic/libc/kernel/uapi/asm-arm64",
    ),
    ArchInfo(
        "riscv64",
        "riscv64-linux-gnu",
        "third_party/android/platform/bionic/libc/kernel/uapi/asm-riscv",
    ),
]

bindgen = Bindgen()
bindgen.opaque_types = OPAQUE_TYPES
bindgen.set_auto_derive_traits(AUTO_DERIVE_TRAITS)
bindgen.set_replacements(REPLACEMENTS)
bindgen.ignore_functions = True
bindgen.no_debug_types = NO_DEBUG_TYPES
bindgen.no_copy_types = NO_COPY_TYPES

for arch in ARCH_INFO:
    bindgen.c_types_prefix = "crate::types"
    bindgen.clang_target = arch.clang_target
    bindgen.raw_lines = RAW_LINES
    bindgen.include_dirs = INCLUDE_DIRS + [arch.include]
    rust_file = "src/starnix/lib/linux_uapi/src/" + arch.name + ".rs"
    bindgen.run(INPUT_FILE, rust_file)
