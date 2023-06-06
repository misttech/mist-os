#!/usr/bin/env -S python3 -B
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os, sys
sys.path.append(os.path.dirname(os.path.abspath(__file__)) + '/..')
from bindgen import Bindgen

RAW_LINES = """
use zerocopy::{AsBytes, FromBytes, FromZeroes};

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

unsafe impl<Storage> FromZeroes for __BindgenBitfieldUnit<Storage>
where
    Storage: FromZeroes,
{
    fn only_derive_is_allowed_to_implement_this_trait() {}
}

#[repr(transparent)]
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Hash, Ord, PartialOrd, AsBytes, FromBytes, FromZeroes)]
pub struct uaddr {
    pub addr: u64,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Hash, Ord, PartialOrd, AsBytes, FromBytes, FromZeroes)]
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
    (r'__IncompleteArrayField', ['AsBytes, FromBytes', 'FromZeroes']),
    (r'__sifields__bindgen_ty_7', ['AsBytes, FromBytes', 'FromZeroes']),
    (
        r'binder_transaction_data__bindgen_ty_2__bindgen_ty_1',
        ['AsBytes', 'FromBytes', 'FromZeroes']),
    (r'binder_transaction_data.*', ['FromBytes', 'FromZeroes']),
    (
        r'bpf_attr__bindgen_ty_(1|3|5|6|7|9|10|11|12|13|15|16|17|18|19)$',
        ['AsBytes', 'FromBytes', 'FromZeroes']),
    (r'bpf_attr.*', ['FromBytes', 'FromZeroes']),
    (r'flat_binder_object.*', ['FromBytes', 'FromZeroes']),
    (r'ip6?t_entry', ['FromBytes', 'FromZeroes']),
    (r'ip6?t_get_entries', ['FromBytes', 'FromZeroes']),
    (r'ip6?t_replace', ['FromBytes', 'FromZeroes']),
    (r'in6_addr', ['AsBytes', 'FromBytes', 'FromZeroes']),
    (r'in6_pktinfo', ['AsBytes', 'FromBytes', 'FromZeroes']),
    (r'inotify_event', ['AsBytes']),
    (r'ip6t_ip6', ['FromBytes', 'FromZeroes']),
    (r'sockaddr_in*', ['AsBytes', 'FromBytes', 'FromZeroes']),
    (r'sock_fprog', ['FromBytes', 'FromZeroes']),
    (r'sysinfo', ['AsBytes']),
    (r'xt_counters_info', ['FromBytes', 'FromZeroes']),
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

    # Add AsBytes/FromBytes/FromZeroes to every copyable struct regardless of
    # name.
    # TODO(https://github.com/rust-lang/rust-bindgen/issues/2170):
    # Remove in favor of bindgen support for custom derives.
    (
        r"\n#\[derive\(Debug, Default, Copy, Clone(, FromBytes)?\)\]\n",
        "\n#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes, FromZeroes)]\n"
    ),

    # Use usize in place of pointers for compat with zerocopy traits. Because
    # the target of the pointer is in userspace anyway, treating it as an opaque
    # pointer is harmless.
    (r'\*mut crate::types::c_void', 'uaddr'),
    (r'([:=]) \*mut ([a-zA-Z_0-9:]*)', '\\1 uref<\\2>')
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
    ArchInfo(
        'riscv64', 'riscv64-linux-gnu',
        'third_party/android/platform/bionic/libc/kernel/uapi/asm-riscv'),
]

bindgen = Bindgen()
bindgen.opaque_types = OPAQUE_TYPES
bindgen.set_auto_derive_traits(AUTO_DERIVE_TRAITS)
bindgen.set_replacements(REPLACEMENTS)
bindgen.ignore_functions = True

for arch in ARCH_INFO:
    bindgen.c_types_prefix = "crate::types"
    bindgen.clang_target = arch.clang_target
    bindgen.raw_lines = RAW_LINES
    bindgen.include_dirs = INCLUDE_DIRS + [arch.include]
    rust_file = 'src/starnix/lib/linux_uapi/src/' + arch.name + '.rs'
    bindgen.run(INPUT_FILE, rust_file)
