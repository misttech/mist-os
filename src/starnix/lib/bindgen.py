#!/usr/bin/env python3
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import re
import subprocess
import sys
import datetime

# All other paths are relative to here (main changes to this directory on startup).
ROOT_PATH = os.path.join(os.path.dirname(__file__), '..', '..', '..')

RUST_DIR = 'prebuilt/third_party/rust/linux-x64/bin'
BINDGEN_PATH = 'prebuilt/third_party/rust_bindgen/linux-x64/bindgen'
FX_PATH = 'scripts/fx'

GENERATED_FILE_HEADER = """// Copyright %d The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
""" % datetime.datetime.now().year


class Bindgen:

    def __init__(self):
        self.clang_target = 'x86_64-pc-linux-gnu'
        self.c_types_prefix = ''
        self.raw_lines = ''
        self.opaque_types = []
        self.include_dirs = []
        self.auto_derive_traits = []
        self.replacements = []
        self.ignore_functions = False
        self.function_allowlist = []
        self.var_allowlist = []
        self.type_allowlist = []

    def set_auto_derive_traits(self, traits_map):
        self.auto_derive_traits = [(re.compile(x[0]), x[1]) for x in traits_map]

    def set_replacements(self, replacements):
        self.replacements = [(re.compile(x[0]), x[1]) for x in replacements]

    def run_bindgen(self, input_file, output_file):
        # Bindgen arguments.
        args = [
            BINDGEN_PATH,
            '--no-layout-tests',
            '--with-derive-default',
            '--explicit-padding',
            '--raw-line',
            GENERATED_FILE_HEADER + self.raw_lines,
            '-o',
            output_file,
        ]

        if self.ignore_functions:
            args.append('--ignore-functions')

        if self.c_types_prefix:
            args.append('--ctypes-prefix=' + self.c_types_prefix)

        args += ['--allowlist-function=' + x for x in self.function_allowlist]
        args += ['--allowlist-var=' + x for x in self.var_allowlist]
        args += ['--allowlist-type=' + x for x in self.type_allowlist]
        args += ['--opaque-type=' + x for x in self.opaque_types]

        args += [input_file]

        # Clang arguments (after the "--").
        args += [
            '--',
            '-target',
            self.clang_target,
            '-nostdlibinc',
            '-D__Fuchsia_API_level__=4294967295',
        ]
        for i in self.include_dirs:
            args += ['-I', i]

        # Need to set the PATH to the prebuilt binary location for it to find rustfmt.
        env = os.environ.copy()
        env['PATH'] = '%s:%s' % (os.path.abspath(RUST_DIR), env['PATH'])
        subprocess.check_call(args, env=env)

    def get_auto_derive_traits(self, line):
        """Returns true if the given line defines a Rust structure with a name
        matching any of the types we need to add FromBytes."""
        if not (line.startswith("pub struct ") or
                line.startswith("pub union ")):
            return None

        # The third word (after the "pub struct") is the type name.
        split = re.split('[ <\(]', line)
        if len(split) < 3:
            return None
        type_name = split[2]

        for (t, traits) in self.auto_derive_traits:
            if t.match(type_name):
                return traits
        return None

    def post_process_rust_file(self, rust_file_name):
        with open(rust_file_name, 'r+') as source_file:
            input_lines = source_file.readlines()
            output_lines = []
            for line in input_lines:
                extra_traits = self.get_auto_derive_traits(line)
                if extra_traits:
                    # Parse existing traits, if any.
                    if len(output_lines) > 0 and output_lines[-1].startswith(
                            '#[derive('):
                        traits = output_lines[-1][9:-3].split(', ')
                        traits.extend(
                            x for x in extra_traits if x not in traits)
                        output_lines.pop()
                    else:
                        traits = extra_traits
                    output_lines.append(
                        '#[derive(' + ', '.join(traits) + ')]\n')
                output_lines.append(line)

            text = "".join(output_lines)
            for (regexp, replacement) in self.replacements:
                text = regexp.sub(replacement, text)

            source_file.seek(0)
            source_file.write(text)

    def run(self, input_file, rust_file):
        os.chdir(ROOT_PATH)

        self.run_bindgen(input_file, rust_file)
        self.post_process_rust_file(rust_file)

        subprocess.check_call([FX_PATH, 'format-code', '--files=' + rust_file])
