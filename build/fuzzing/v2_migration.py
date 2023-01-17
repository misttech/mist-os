#! /usr/bin/env python3

# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import functools
import os
import re
import subprocess
import sys
from collections import deque
from string import Template


def main():
    """Finds and replaces v1 GN fuzzing templates with corresponding v2 templates.

    Given a list of directories, this script will make two passes:

      1). On the first pass, it will identify the build files in those directories that have fuzzing
          related targets and imports, and collect any details needed to generate fuzzer components.

      2). On the second pass, it will generate build files with v1 fuzzing imports and targets
          replaced by v2 ones. If the "dry run" flag was not specified, it will write these build
          files back and also write the corresponding fuzzer component manifest source files.
  """
    parser = argparse.ArgumentParser(
        description='Convert v1 GN fuzzing targets to v2.')
    parser.add_argument(
        '-d', '--dry-run', action='store_true', help='skip writing changes')
    parser.add_argument(
        'roots',
        nargs='+',
        metavar='dirs',
        help='directories to search for targets')
    args = parser.parse_args()

    for root in args.roots:
        if not os.path.isdir(root):
            print('error: invalid argument: ' + root)
            parser.print_help()
            return -1

    # Scan for BUILD.gn files with v1 fuzzing imports and targets.
    converter = FuzzingTemplateConverter()
    buildfiles = []
    imports = {}
    n = 0
    for root in args.roots:
        for dirpath, dirs, files in os.walk(root):
            if 'out' in dirs:
                dirs.remove('out')
            dirs = [d for d in dirs if not d.startswith('.')]
            for filename in files:
                if filename != 'BUILD.gn':
                    continue
                buildfile = os.path.join(dirpath, filename)
                if converter.on_buildfile(buildfile):
                    buildfiles.append(buildfile)
                    imports[buildfile] = converter.imports
                    n += 1
    print('')
    print(f'[+] Found {n} fuzzing-related BUILD.gn files.')

    # Replace v1 fuzzing imports and targets in affected BUILD.gn files.
    print('')
    converter.mode = 'dry-run' if args.dry_run else 'modify'
    i = 1
    for buildfile in buildfiles:
        print(f'[{i}/{n}] {buildfile}')
        converter.imports = imports[buildfile]
        converter.on_buildfile(buildfile)
        if not args.dry_run:
            subprocess.check_output(
                ['fx', 'gn', 'format', buildfile],
                stderr=subprocess.STDOUT,
                encoding="utf8")
        i += 1

    print('')
    print('[+] Done!')
    return 0


class FuzzingTemplateConverter(object):
    """Processes BUILD.gn files to find and replace v1 fuzzing imports and targets with v2 ones.

  The converter is given a sequence of BUILD.gn files to process, depending on its currently
  configured `mode`. When mode is 'scan', it collects fuzzer component details. When it is 'dry-run'
  or 'modify', it generates GN output for fuzzer executables, components, and packages. Furthermore,
  when it is 'modify', it writes this GN output back to the build files and generates CML files.

  Attributes:
    mode: String indicating one of 'scan', 'dry-run' or 'modify', as set by the caller.
    imports: Set of strings indicating any gni files to be imported by the current build file.
    components: Map of fuzzer labels to scopes defining component "deps" and manifest "args".
    input_lines: List of strings representing the original lines of the current build file.
    output_lines: List of strings representing the replacement lines for the current build file.
    dirpath: String representing the directory of the current build file.
  """

    def __init__(self):
        self.mode = 'scan'
        self.imports = set()
        self.components = {}

        self.input_lines = None
        self.output_lines = []
        self.dirpath = None

    def on_buildfile(self, pathname):
        """Take mode-specific actions on the BUILD.gn file given by `pathname`.

    Reads the file line by line and identifies fuzzing-related imports and targets. Since the
    correct v2 replacements for v1 imports and targets may depend on details later in a build file
    or in a different build file altogether, callers will typically make two passes:

      * If the mode is 'scan', it collects information about fuzzing imports and targets
      * Otherwise, it generates output for each build file with v1 fuzzing imports and targets
        replaced by v2. If the mode is 'modify', it writes these to the build file.

    Returns whether the file has fuzzing related targets.
    """
        with open(pathname, 'r') as file:
            lines = [line.rstrip() for line in file.readlines()]
            self.input_lines = deque(lines)
        self.output_lines = []
        self.dirpath = os.path.dirname(pathname)

        if self.mode == 'scan':
            self.imports = set()

        modified = False
        while self.input_lines:
            line = self.input_lines.popleft()
            if (self.on_fuzzing_imports(line) or self.on_fuzzer(line) or
                    self.on_cpp_fuzzer(line) or self.on_rustc_fuzzer(line) or
                    self.on_fidl_protocol_fuzzer(line) or
                    self.on_fuzzer_package(line)):
                modified = True
            else:
                self.output_lines.append(line)

        if self.mode == 'modify':
            with open(pathname, 'w') as buildfile:
                for line in self.output_lines:
                    print(line, file=buildfile)

        return modified

    def on_fuzzing_imports(self, line):
        """Take mode-specific actions on a GN imports in the given `line`.

    Does nothing if the given `line` is not a fuzzing-related GN import. If it is, then:
      If the current mode is 'scan', this will update the converter's list of needed imports.
      Otherwise, it adds all needed imports to the converter's output, and clears its imports.

    Returns whether the line is a fuzzing-related GN import.
    """
        m = re.match(r'\s*import\("(.*)"\)', line)

        if not m:
            return False

        if m[1] == '//build/fuzzing/fuzzer.gni' or m[
                1] == '//build/fuzzing/fuzzer_package.gni':
            if self.mode == 'scan':
                self.imports.add('//build/fuzz.gni')
            else:
                self.output_lines.extend(
                    [f'import("{gni}")' for gni in self.imports])
                self.imports = set()
            return True

        return False

    def on_fuzzer(self, line, target_type='fuzzer'):
        """Take mode-specific actions on a fuzzer target in the given `line`.

    Does nothing if the given `line` is not a fuzzer target. If it is, then:
      If the current mode is 'scan', this will determine component related details for the fuzzer.
      Otherwise, it replaces v1 fuzzing targets with v2 and adds them to the converter's output.

    Returns whether the line is the start of a fuzzer target with the given `target_type`.
    """
        target_name, found = self.read_target(target_type, line)
        if not found:
            return False

        fuzzer_label = source_absolute(self.dirpath, ':' + target_name)
        output_name = target_name
        args = []
        deps = []
        fuzzer_scope = []

        while self.target_scope:
            line = self.target_scope.popleft()

            invoker_output_name, found = read_string_variable(
                'output_name', line)
            if found:
                output_name = invoker_output_name

            corpus, found = read_string_variable('corpus', line)
            if found:
                if self.mode == 'scan':
                    corpus_label = source_absolute(self.dirpath, corpus)
                    deps.append(corpus_label)
                    args.append('data/' + corpus_label.replace('//', ''))
                continue

            dictionary, found = read_string_variable('dictionary', line)
            if found:
                if self.mode == 'scan':
                    deps.append(fuzzer_label + '-dictionary')
                    self.imports.add('//build/dist/resource.gni')
                    args.append('data/' + os.path.basename(dictionary))
                else:
                    dictionary_scope = []
                    dictionary_scope += make_string_list(
                        'sources', [dictionary])
                    dictionary_scope += make_string_list(
                        'outputs', [r'data/{{source_file_part}}'])
                    self.write_target(
                        'resource', target_name + '-dictionary',
                        dictionary_scope)
                continue

            options, found = self.read_string_list('options', line)
            if found:
                args.extend([f'-{option}' for option in options])
                continue

            fuzzer_scope.append(line)

        if self.mode == 'scan':
            self.components[fuzzer_label] = {
                "args": [f'test/{output_name}'] + args,
                "deps": deps,
            }
        elif target_type == 'fuzzer':
            self.write_target(
                'fuchsia_library_fuzzer', target_name, fuzzer_scope)
        else:
            self.write_target(target_type, target_name, fuzzer_scope)
        return True

    def on_cpp_fuzzer(self, line):
        """Like `on_fuzzer`, but for the `cpp_fuzzer` template that aliases `fuzzer`."""
        return self.on_fuzzer(line, target_type='cpp_fuzzer')

    def on_rustc_fuzzer(self, line):
        """Records component details for `rustc_fuzzers`."""
        return self.on_fuzzer(line, target_type='rustc_fuzzer')

    def on_fidl_protocol_fuzzer(self, line):
        """Records component details for `fidl_protocol_fuzzer`."""
        if self.mode == 'scan':
            return self.on_fuzzer(line, target_type='fidl_protocol_fuzzer')

    def on_fuzzer_package(self, line):
        """Take mode-specific actions on a `fuzzer_package` target in the given `line`.

    If the given `line` is a `fuzzer_package` target, and the current mode is not 'scan', it adds
    v2 fuzzer components and a v2 fuzzer package to the converter's output. Additionally, if the
    mode is 'modify', it will create the component manifest source files for those components.

    Returns whether the line is the start of a `fuzzer_package` target.
    """
        target_name, found = self.read_target('fuzzer_package', line)
        if not found:
            return False

        if self.mode == 'scan':
            return True

        package_scope = []
        fuzzer_labels = []
        fuzz_host = False
        while self.target_scope:
            line = self.target_scope.popleft()

            found = False
            for lang in ['cpp', 'rust']:
                labels, found = self.read_string_list(lang + '_fuzzers', line)
                if found:
                    break
            if found:
                fuzzer_labels += labels
                package_scope += make_string_list(
                    lang + '_fuzzer_components', [
                        ':' + component_name(fuzzer_label)
                        for fuzzer_label in labels
                    ])
                continue

            value, found = read_bool_variable('fuzz_host', line)
            if found:
                fuzz_host = value
                continue

            package_scope.append(line)

        # Create the fuzzer components.
        for fuzzer_label in fuzzer_labels:
            component = self.components[source_absolute(
                self.dirpath, fuzzer_label)]
            manifest = manifest_path(self.dirpath, fuzzer_label)
            create_manifest(
                self.dirpath, manifest, component['args'],
                self.mode != 'modify')
            component_scope = []
            component_scope.append(f'manifest = "{manifest}"')
            component_deps = [fuzzer_label]
            component_deps.extend(
                [
                    source_relative(self.dirpath, dep)
                    for dep in component['deps']
                ])
            component_scope += make_string_list('deps', component_deps)
            self.write_target(
                'fuchsia_fuzzer_component', component_name(fuzzer_label),
                component_scope)

        # Create the fuzzer package and a host fuzzer group, if requested.
        if fuzz_host:
            group_scope = ['testonly = true']
            group_scope += make_string_list('deps', fuzzer_labels)

            self.output_lines.append('if (is_fuchsia) {')
            self.write_target(
                'fuchsia_fuzzer_package', target_name, package_scope)
            self.output_lines.append('} else {')
            self.write_target('group', target_name, group_scope)
            self.output_lines.append('}')

        else:
            self.write_target(
                'fuchsia_fuzzer_package', target_name, package_scope)

        return True

    def read_target(self, target_type, line):
        """Attempts to parse a GN target of the given `target_type` starting in the given `line`.

    If the given `line` is a `target_type` target, reads lines from the converter's input up to the
    closing curly brace into the converter's target scope.

    Returns a tuple of the target name and whether the line is the start of a `target_type` target.
    """
        m = re.match(rf'\s*{target_type}\("([^\$]*)"\) {{', line)
        if not m:
            return (None, False)

        target_name = m[1]
        target_scope = deque([])
        depth = 1
        while self.input_lines:
            line = self.input_lines.popleft()
            depth += line.count('{') - line.count('}')
            if depth <= 0:
                break
            target_scope.append(line)
        # Consume any following blank lines
        while self.input_lines and self.input_lines[0] == '':
            self.input_lines.popleft()
        self.target_scope = target_scope
        return (target_name, True)

    def read_string_list(self, name, line):
        """Attempts to parse a GN list of the given `name` starting in the given `line`.

    If the given `line` is a `name`d list, reads lines from the converter's target scope up to the
    closing square bracket. Each line representing a string value is added to a list of items.

    Returns a tuple of the items and whether the line is the start of a `name`d list.
    """
        m = re.match(rf'\s*{name} \+?= \[\]', line)
        if m:
            return ([], True)
        m = re.match(rf'\s*{name} \+?= \[ "(.*)" \]', line)
        if m:
            return ([m[1]], True)
        m = re.match(rf'\s*{name} \+?= \[', line)
        if not m:
            return (None, False)

        items = []
        depth = 1
        while self.target_scope and depth > 0:
            line = self.target_scope.popleft()
            depth += line.count('[') - line.count(']')
            m = re.match(r'\s*"(.*)",', line)
            if m:
                items.append(m[1])

        return (items, True)

    def write_target(self, target_type, target_name, target_scope):
        """Adds lines for a GN target to the converter's output."""
        self.output_lines.append(f'{target_type}("{target_name}") {{')
        self.output_lines.extend(target_scope)
        self.output_lines.append('}')
        self.output_lines.append('')


def read_bool_variable(name, line):
    """Attempts to parse `line` as a GN boolean variable of the given `name`.

  Returns a tuple of the value and whether the line is a `name`d boolean value.
  """
    m = re.match(rf'\s*{name} = (.*)', line)
    return (m[1] == 'true', True) if m else (None, False)


def read_string_variable(name, line):
    """Attempts to parse `line` as a GN string variable of the given `name`.

  Returns a tuple of the value and whether the line is a `name`d string value.
  """
    m = re.match(rf'\s*{name} = "(.*)"', line)
    return (m[1], True) if m else (None, False)


def make_string_list(name, items):
    """Converts a list of items in to a list of lines needed to specify the list in GN.

    Example:
      make_string_list('things', []) => [ 'things = []' ]
      make_string_list('things', [ 'foo' ]) => [ 'things = [ "foo" ]' ]
      make_string_list('things', [ 'foo', 'bar', 'baz' ]) =>
        [ 'things = [', '"foo",', '"bar",', '"baz",', ']' ]
    """
    if len(items) == 0:
        return [f'{name} = []']
    if len(items) == 1:
        return [f'{name} = [ "{items[0]}" ]']
    string_list = [f'{name} = [']
    string_list.extend([f'"{item}",' for item in items])
    string_list.append(']')
    return string_list


@functools.cache
def source_root(dirpath):
    """Find the absolute path to the GN source root.

  Example:
    source_root('/path/to/fuchsia/examples/fuzzer') => '/path/to/fuchsia'

  Raises a `ValueError` if the .gn file cannot be found in the current or any parent directory.
  """
    root = os.path.realpath(dirpath)
    while not os.path.isfile(os.path.join(root, '.gn')):
        if root == '/':
            raise ValueError(
                f'"{dirpath}" does not appear to be part of a GN project')
        root = os.path.dirname(root)
    return root


def source_absolute(dirpath, relative_label):
    """Converts a relative GN label to be absolute.

  Example:
    source_absolute('~/fuchsia/examples/fuzzer', 'cpp:crash_fuzzer') =>
      '//examples/fuzzers/cpp:crash_fuzzer'
  """
    if relative_label.startswith('//'):
        return relative_label
    absolute_label = '//' + os.path.relpath(dirpath, source_root(dirpath))
    parts = relative_label.split(':')
    if parts[0]:
        absolute_label = os.path.join(absolute_label, parts[0])
    if len(parts) > 1:
        absolute_label += f':{parts[1]}'
    return absolute_label


def source_relative(dirpath, absolute_label):
    """Converts an absolute GN label to be relative.

  Example:
    source_relative('~/fuchsia/examples/fuzzer', '//examples/fuzzers/cpp:crash_fuzzer') =>
      'cpp:crash_fuzzer'
  """
    if not absolute_label.startswith('//'):
        return absolute_label
    parts = absolute_label.split(':')
    abspath = os.path.join(source_root(dirpath), parts[0][2:])
    relative_label = os.path.relpath(abspath, dirpath)
    if len(parts) > 1:
        relative_label += f':{parts[1]}'
    return relative_label


def component_name(fuzzer_label):
    """Generates a component name from a GN label.

  Example:
    component_name('cpp:crash_fuzzer') => 'crash-fuzzer-component'
  """
    pos = fuzzer_label.rfind(':')
    if pos == -1:
        name = fuzzer_label
    else:
        name = fuzzer_label[(pos + 1):]
    return name.replace('_', '-') + '-component'


def manifest_path(dirpath, relative_label):
    """Generates a path to a component manifest source file from a relative GN label.

  Example:
    manifest_path('~/fuchsia/examples/fuzzer', 'cpp:crash_fuzzer') =>
      '~/fuchsia/examples/fuzzer/cpp/meta/crash_fuzzer.cml'
  """
    srcpath = '//' + os.path.relpath(dirpath, source_root(dirpath))
    absolute_label = source_absolute(dirpath, relative_label)
    label_info = absolute_label.split(':')
    label_info_dir = os.path.normpath(label_info[0])
    label_info_name = label_info[1] if len(
        label_info) > 1 else os.path.basename(label_info_dir)
    srcpath = (os.path.relpath(label_info_dir, srcpath))
    path = os.path.normpath(
        os.path.join(srcpath, 'meta', f'{label_info_name}.cml'))
    return path


def create_manifest(dirpath, relpath, args, dry_run):
    """Creates a fuzzer component manifest source file.

  The generated manifest will simply include libFuzzer's default shard, and augment it with the
  specific programs arguments needed by the runner executable.

  Raises a `ValueError` if the requested manifest already exists.
  """
    pathname = os.path.join(dirpath, relpath)
    os.makedirs(os.path.dirname(pathname), exist_ok=True)
    if os.path.isfile(pathname):
        raise ValueError(f'manifest already exists: {pathname}')

    lines = ['        args: [']
    if len(args) == 1:
        lines[0] = lines[0] + f' "{args[0]}" ],'
    else:
        lines.extend([f'            "{arg}",' for arg in args])
        lines.append('        ],')

    substitutions = {'args': '\n'.join(lines)}

    script_dir = os.path.dirname(os.path.realpath(__file__))
    with open(os.path.join(script_dir, 'fuchsia_fuzzer_component_template.txt'),
              'r') as txt:
        template = Template(txt.read())
        output = template.substitute(substitutions)
        if not dry_run:
            with open(pathname, 'w+') as cml:
                print(output, end='', file=cml)


if __name__ == '__main__':
    sys.exit(main())
