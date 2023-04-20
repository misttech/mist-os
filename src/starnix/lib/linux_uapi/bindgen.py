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


# Additional traits that should be added to types matching the regexps.
AUTO_DERIVE_TRAITS = [
    (re.compile(x[0]), x[1]) for x in [
        (r'__IncompleteArrayField', ['AsBytes, FromBytes']),
        (r'binder_transaction_data.*', ['FromBytes']),
        (r'bpf_attr.*', ['FromBytes']),
        (r'flat_binder_object.*', ['FromBytes']),
        (r'ip6?t_entry', ['FromBytes']),
        (r'ip6?t_get_entries', ['FromBytes']),
        (r'ip6?t_replace', ['FromBytes']),
        (r'in6_addr', ['FromBytes']),
        (r'ip6t_ip6', ['FromBytes']),
        (r'sysinfo', ['AsBytes']),
        (r'xt_counters_info', ['FromBytes']),
    ]
];


# General replacements to apply to the contents of the file. These are tuples of
# compiled regular expressions + the thing to replace matches with.
GENERAL_REPLACEMENTS = [
    (re.compile(x[0]), x[1]) for x in [
        # Replace xt_counters pointers with u64.
        (r'\*(const|mut) xt_counters', 'u64'),

        # Use CStr to represent constant C strings. The inputs look like:
        #   pub const FS_KEY_DESC_PREFIX: &[u8; 9usize] = b"fscrypt:\0";
        (
            r': &\[u8; [0-9]+(usize)?\] = (b".*\\0");\n',
            ": &'static std::ffi::CStr = "
            "unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(\\2) };\n"
        ),

        # Change `__IncompleteArrayField` representation to `transparent`, which is necessary to
        # allow it to derive `AsBytes`.
        # TODO(https://github.com/google/zerocopy/issues/10): Remove this once zerocopy is updated
        # to allow `AsBytes` for generic structs with `repr(C)`.
        (
            r'#\[repr\(C\)\]\n'
            r'#\[derive\((([A-Za-z]+, )*[A-Za-z]+)\)\]\n'
            r'pub struct __IncompleteArrayField',
            '#[repr(transparent)]\n'
            '#[derive(\\1)]\n'
            'pub struct __IncompleteArrayField'
        ),

        # Add AsBytes/FromBytes to every copyable struct regardless of name.
        # TODO(https://github.com/rust-lang/rust-bindgen/issues/2170):
        # Remove in favor of bindgen support for custom derives.
        (
            r"\n#\[derive\(Debug, Default, Copy, Clone(, FromBytes)?\)\]\n",
            "\n#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes)]\n"
        )
    ]
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
        'src/starnix/lib/linux_uapi/src/x86_64.rs'),
    ArchInfo(
        'arm64', 'crate::arm64_types', 'aarch64-linux-gnu',
        'third_party/android/platform/bionic/libc/kernel/uapi/asm-arm64',
        'src/starnix/lib/linux_uapi/src/arm64.rs'),
]

# Cross-architecture include paths (the ArchInfo class also has an arch-specific one to add).
INCLUDE_DIRS = [
    'third_party/android/platform/bionic/libc/kernel/uapi',
    'third_party/android/platform/bionic/libc/kernel/android/uapi',
    'src/starnix/kernel/vdso',
    'src/starnix/lib/linux_uapi/stub',
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

    args += ["src/starnix/lib/linux_uapi/wrapper.h"]

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


def get_auto_derive_traits(line):
    """Returns true if the given line defines a Rust structure with a name
    matching any of the types we need to add FromBytes."""
    if not (line.startswith("pub struct ") or line.startswith("pub union ")):
        return None

    # The third word (after the "pub struct") is the type name.
    split = re.split('[ <\(]', line)
    if len(split) < 3:
        return None
    type_name = split[2]

    for (t, traits) in AUTO_DERIVE_TRAITS:
        if t.match(type_name):
            return traits
    return None


def post_process_rust_file(rust_file_name):
    with open(rust_file_name, 'r+') as source_file:
        input_lines = source_file.readlines()
        output_lines = []
        for line in input_lines:
            extra_traits = get_auto_derive_traits(line)
            if extra_traits:
                # Parse existing traits, if any.
                if len(output_lines) > 0 and output_lines[-1].startswith('#[derive('):
                    traits = output_lines[-1][9:-3].split(', ')
                    traits.extend(x for x in extra_traits if x not in traits)
                    output_lines.pop();
                else:
                    traits = extra_traits
                output_lines.append("#[derive(" + ", ".join(traits) + ")]\n")
            output_lines.append(line)

        text = "".join(output_lines)
        for (regexp, replacement) in GENERAL_REPLACEMENTS:
            text = regexp.sub(replacement, text)

        source_file.seek(0)
        source_file.write(text)


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
