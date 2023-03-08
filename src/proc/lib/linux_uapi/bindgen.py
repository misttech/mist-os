#!/usr/bin/env python3
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import re
import subprocess
import sys

# All other paths are relative to here (main changes to this directory on startup).
ROOT_PATH = os.path.join(os.path.dirname(__file__), '..', '..', '..', '..')

RUST_DIR = 'prebuilt/third_party/rust/linux-x64/bin'
BINDGEN_PATH = 'prebuilt/third_party/rust_bindgen/linux-x64/bindgen'
FX_PATH = 'scripts/fx'

# Tell bindgen not to produce records for these types.
OPAQUE_TYPES = [
    '__sighandler_t',
    '__sigrestore_t',
    'group_filter.*',
    'sigevent',
]

# Adds a derive for 'FromBytes' for each type name matching these regexps.
AUTO_DERIVE_FROM_BYTES_FOR = [
    re.compile(r) for r in [
        'binder_transaction_data.*',
        'flat_binder_object.*',
        'bpf_attr.*',
        '__IncompleteArrayField',
        'ipt_get_entries',
        'ipt_replace',
        'ipt_entry',
        'ip6t_ip6',
        'in6_addr',
        'xt_counters_info',
        'ip6t_get_entries',
        'ip6t_entry',
        'ip6t_replace',
    ]
]

# General replacements to apply to the contents of the file. These are tuples of
# compiled regular expressions + the thing to replace matches with.
GENERAL_REPLACEMENTS = [
    # Replace xt_counters pointers with u64.
    (re.compile('\*(const|mut) xt_counters'), 'u64'),

    # Use CStr to represent constant C strings. The inputs look like:
    #   pub const FS_KEY_DESC_PREFIX: &[u8; 9usize] = b"fscrypt:\0";
    (
        re.compile(': &\[u8; [0-9]+usize\] = (b".*)\\\\0";$'),
        """: &'static std::ffi::CStr = unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(\\1\\\\0") };"""
    ),

    # Add AsBytes/FromBytes to every copyable struct regardless of name.
    # TODO(https://github.com/rust-lang/rust-bindgen/issues/2170):
    # Remove in favor of bindgen support for custom derives.
    (
        re.compile("^#\[derive\(Debug, Default, Copy, Clone\)\]$"),
        "#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes)]"),
]


class ArchInfo:

    def __init__(self, name, type_prefix, clang_target, include, rust_file):
        self.name = name  # Our internal arch name.
        self.type_prefix = type_prefix  # Crate prefix name.
        self.clang_target = clang_target  # Clang "triple" name for this arch.
        self.include = include  # Include directory for the arch-specific uapi files.
        self.rust_file = rust_file  # Output Rust file to generate.


ARCH_INFO = [
    ArchInfo(
        'x86_64', 'crate::x86_64_types', 'x86_64-pc-linux-gnu',
        'third_party/android/platform/bionic/libc/kernel/uapi/asm-x86',
        'src/proc/lib/linux_uapi/src/x86_64.rs'),
    ArchInfo(
        'arm64', 'crate::arm64_types', 'aarch64-linux-gnu',
        'third_party/android/platform/bionic/libc/kernel/uapi/asm-arm64',
        'src/proc/lib/linux_uapi/src/arm64.rs'),
]

# Cross-architecture include paths (the ArchInfo class also has an arch-specific one to add).
INCLUDE_DIRS = [
    'third_party/android/platform/bionic/libc/kernel/uapi',
    'third_party/android/platform/bionic/libc/kernel/android/uapi',
    'src/proc/lib/linux_uapi/stub',
]

RAW_LINES = """// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_snake_case)]

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


def run_bindgen(type_prefix, raw_line, output_file, clang_target, include_dirs):
    # Bindgen arguments.
    args = [
        BINDGEN_PATH,
        '--no-layout-tests',
        '--ignore-functions',
        '--with-derive-default',
        '--explicit-padding',
        '--ctypes-prefix=' + type_prefix,
        '--raw-line',
        raw_line,
        '-o',
        output_file,
    ]
    for t in OPAQUE_TYPES:
        args += ['--opaque-type=' + t]

    args += ["src/proc/lib/linux_uapi/wrapper.h"]

    # Clang arguments (after the "--").
    args += [
        '--',
        '-target',
        clang_target,
        '-nostdlibinc',
    ]
    for i in include_dirs:
        args += ['-I', i]

    # Need to set the PATH to the prebuilt binary location for it to find rustfmt.
    env = os.environ.copy()
    env['PATH'] = '%s:%s' % (os.path.abspath(RUST_DIR), env['PATH'])
    subprocess.check_call(args, env=env)


def is_from_bytes_matching_type_decl_line(line):
    """Returns true if the given line defines a Rust structure with a name
    matching any of the types we need to add FromBytes."""
    if not (line.startswith("pub struct ") or line.startswith("pub union ")):
        return False

    # The third word (after the "pub struct") is the type name.
    split = re.split('[ <\(]', line)
    if len(split) < 3:
        return False
    type_name = split[2]

    for t in AUTO_DERIVE_FROM_BYTES_FOR:
        if t.match(type_name):
            return True
    return False


def post_process_rust_file(rust_file_name):
    with open(rust_file_name, 'r+') as source_file:
        input_lines = source_file.readlines()
        output_lines = []
        for line in input_lines:
            if is_from_bytes_matching_type_decl_line(line):
                # Add "FromBytes" to every matching type.
                if len(output_lines) > 0 and output_lines[-1].startswith(
                        '#[derive('):
                    if not "FromBytes" in output_lines[-1]:
                        # Insert annotation into previous derive line (note last char is a newline).
                        output_lines[
                            -1] = output_lines[-1][:-3] + ", FromBytes)]\n"
                else:
                    # No derive line, insert a new one.
                    output_lines += ["#[derive(FromBytes)]\n"]
            else:
                for (regexp, replacement) in GENERAL_REPLACEMENTS:
                    line = regexp.sub(replacement, line)
            output_lines += [line]

        source_file.seek(0)
        source_file.write("".join(output_lines))


def generate_platform_uapi(arch_info):
    run_bindgen(
        arch_info.type_prefix, RAW_LINES % arch_info.type_prefix,
        arch_info.rust_file, arch_info.clang_target,
        INCLUDE_DIRS + [arch_info.include])

    post_process_rust_file(arch_info.rust_file)

    subprocess.check_call(
        [FX_PATH, 'format-code', '--files=' + arch_info.rust_file])


def main():
    # Make all other paths relative to the root.
    os.chdir(ROOT_PATH)
    for arch in ARCH_INFO:
        generate_platform_uapi(arch)


if __name__ == '__main__':
    sys.exit(main())
