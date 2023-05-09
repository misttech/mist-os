#!/usr/bin/env -S python3 -B
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os, sys
sys.path.append(os.path.dirname(os.path.abspath(__file__)) + '/..')
from bindgen import Bindgen

RAW_LINES = """
use zerocopy::{AsBytes, FromBytes};

pub use %s::*;

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
"""

# Tell bindgen not to produce records for these types.
OPAQUE_TYPES = [
    '__sighandler_t',
    '__sigrestore_t',
    'group_filter.*',
    'sigevent',
]

# Cross-architecture include paths (the ArchInfo class also has an arch-specific one to add).
INCLUDE_DIRS = [
    'third_party/android/platform/bionic/libc/kernel/uapi',
    'third_party/android/platform/bionic/libc/kernel/android/uapi',
    'src/starnix/kernel/vdso',
    'src/starnix/lib/linux_uapi/stub',
]

# Additional traits that should be added to types matching the regexps.
AUTO_DERIVE_TRAITS = [
    (r'__IncompleteArrayField', ['AsBytes, FromBytes']),
    (r'__sifields__bindgen_ty_7', ['AsBytes, FromBytes']),
    (r'binder_transaction_data.*', ['FromBytes']),
    (r'bpf_attr.*', ['FromBytes']),
    (r'flat_binder_object.*', ['FromBytes']),
    (r'ip6?t_entry', ['FromBytes']),
    (r'ip6?t_get_entries', ['FromBytes']),
    (r'ip6?t_replace', ['FromBytes']),
    (r'in6_addr', ['AsBytes', 'FromBytes']),
    (r'in6_pktinfo', ['AsBytes', 'FromBytes']),
    (r'inotify_event', ['AsBytes']),
    (r'ip6t_ip6', ['FromBytes']),
    (r'sockaddr_in*', ['AsBytes', 'FromBytes']),
    (r'sock_fprog', ['FromBytes']),
    (r'sysinfo', ['AsBytes']),
    (r'xt_counters_info', ['FromBytes']),
]

# General replacements to apply to the contents of the file. These are tuples of
# compiled regular expressions + the thing to replace matches with.
REPLACEMENTS = [
    # Replace xt_counters pointers with u64.
    (r'\*(const|mut) xt_counters', 'u64'),

    # Use CStr to represent constant C strings. The inputs look like:
    #   pub const FS_KEY_DESC_PREFIX: &[u8; 9usize] = b"fscrypt:\0";
    (
        r': &\[u8; [0-9]+(usize)?\] = (b".*\\0");\n',
        ": &'static std::ffi::CStr = "
        "unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(\\2) };\n"),

    # Change `__IncompleteArrayField` representation to `transparent`, which is necessary to
    # allow it to derive `AsBytes`.
    # TODO(https://github.com/google/zerocopy/issues/10): Remove this once zerocopy is updated
    # to allow `AsBytes` for generic structs with `repr(C)`.
    (
        r'#\[repr\(C\)\]\n'
        r'#\[derive\((([A-Za-z]+, )*[A-Za-z]+)\)\]\n'
        r'pub struct __IncompleteArrayField', '#[repr(transparent)]\n'
        '#[derive(\\1)]\n'
        'pub struct __IncompleteArrayField'),

    # Add AsBytes/FromBytes to every copyable struct regardless of name.
    # TODO(https://github.com/rust-lang/rust-bindgen/issues/2170):
    # Remove in favor of bindgen support for custom derives.
    (
        r"\n#\[derive\(Debug, Default, Copy, Clone(, FromBytes)?\)\]\n",
        "\n#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes)]\n"),

    # The next two allow clients to apply FromBytes to a given struct.  Because the
    # target of the pointer is in userspace anyway, treating it as an opaque
    # pointer is harmless.
    (r'\*mut crate::x86_64_types::c_void', 'usize'),
    (r'\*mut crate::arm64_types::c_void', 'usize'),
    (r'\*mut sock_filter', 'usize')
]

INPUT_FILE = 'src/starnix/lib/linux_uapi/wrapper.h'


class ArchInfo:

    def __init__(self, name, clang_target, include):
        self.name = name  # Our internal arch name.
        self.clang_target = clang_target  # Clang "triple" name for this arch.
        self.include = include  # Include directory for the arch-specific uapi files.


ARCH_INFO = [
    ArchInfo(
        'x86_64', 'x86_64-pc-linux-gnu',
        'third_party/android/platform/bionic/libc/kernel/uapi/asm-x86'),
    ArchInfo(
        'arm64', 'aarch64-linux-gnu',
        'third_party/android/platform/bionic/libc/kernel/uapi/asm-arm64'),
]

bindgen = Bindgen()
bindgen.opaque_types = OPAQUE_TYPES
bindgen.set_auto_derive_traits(AUTO_DERIVE_TRAITS)
bindgen.set_replacements(REPLACEMENTS)
bindgen.ignore_functions = True

for arch in ARCH_INFO:
    type_prefix = 'crate::' + arch.name + '_types'
    bindgen.c_types_prefix = type_prefix
    bindgen.clang_target = arch.clang_target
    bindgen.raw_lines = RAW_LINES % type_prefix
    bindgen.include_dirs = INCLUDE_DIRS + [arch.include]
    rust_file = 'src/starnix/lib/linux_uapi/src/' + arch.name + '.rs'
    bindgen.run(INPUT_FILE, rust_file)
