#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script is not be used everytime. Just to help create the first skeleton.

import re
import os
import argparse


def parse_syscall_tbl(tbl_file):
    syscall_info = []
    with open(tbl_file, "r") as f:
        for line in f:
            items = re.split(r"\t+", line.strip())
            if len(items) >= 3:
                syscall_number = int(items[0])
                syscall_name = items[1]
                syscall_abi = items[2]
                syscall_description = items[3] if len(items) >= 4 else None
                syscall_info.append(
                    (
                        syscall_number,
                        syscall_name,
                        syscall_abi,
                        syscall_description,
                    )
                )
    return syscall_info


def find_syscalls(source_dir):
    syscalls = []
    file_count = sum(
        file.endswith(".c")
        for _, _, files in os.walk(source_dir)
        for file in files
    )
    processed_files = 0
    print("Parsing .c files:")
    pattern = r"SYSCALL_DEFINE[0-6]\((?P<name>\w+)(?:,\s*(?P<args>.*?))?\)"
    syscall_regex = re.compile(pattern)
    for root, _, files in os.walk(source_dir):
        for file in files:
            if file.endswith(".c"):
                with open(os.path.join(root, file), "r") as f:
                    lines = f.readlines()
                    lines = [line.strip() for line in lines]
                    lines = " ".join(lines)
                    matches = syscall_regex.findall(lines)
                    for match in matches:
                        syscall_name = match[0]
                        syscall_args = match[1:]
                        syscalls.append((syscall_name, syscall_args))
                processed_files += 1
                print(
                    f"Processed {processed_files}/{file_count} files", end="\r"
                )
    print("\nParsing completed.")
    return syscalls


def generate_fidls(syscall_info):
    fidl_definitions = []
    dummy_generated = False
    blank_generated = False
    interval_generated = False
    syscall_count = 0
    for syscall_number, syscall_abi, syscall_name, entry_point in syscall_info:
        #print("{}-{}".format(syscall_name, entry_point))
        if entry_point == "sys_ni_syscall":
            fidl_definitions.append(
                f"\n// STUB (Kernel NI) {syscall_number} Not Implemented syscall\nstrict A{syscall_number:04}NotImpl() -> () error int64;\n"
            )
            syscall_count += 1
            continue
        if not entry_point:
            camel_case_name = "".join(
                word.capitalize() for word in syscall_name.split("_")
            )
            fidl_definitions.append(
                f"\n// STUB (Not entry point) {syscall_number}\nstrict A{syscall_number:04}{camel_case_name}() -> () error int64;\n"
            )
            syscall_count += 1
            continue
        if syscall_number > 334 and not blank_generated:
            i = 335
            while i <= 386:
                fidl_definitions.append(
                    f"\n// STUB (Blank Syscall) {i}\nstrict A{i:04}Blank() -> () error int64;\n"
                )
                i += 1
                syscall_count += 1
            blank_generated = True
        if syscall_number > 387 and not dummy_generated:
            # don't use numbers 387 through 423,
            i = 387
            while i <= 423:
                fidl_definitions.append(
                    f"\n// STUB (Don't use) {i}\nstrict A{i:04}DontUse() -> () error int64;\n"
                )
                i += 1
                syscall_count += 1
            dummy_generated = True
        if syscall_number > 453 and not interval_generated:
            # don't use numbers 454 through 512,
            i = 454
            while i <= 511:
                fidl_definitions.append(
                    f"\n// STUB (Blank syscall) {i}\nstrict A{i:04}Blank() -> () error int64;\n"
                )
                i += 1
                syscall_count += 1
            interval_generated = True
        if syscall_abi == "x32":
            syscall_name = f"compat_{syscall_name}"
        camel_case_name = "".join(
            word.capitalize() for word in syscall_name.split("_")
        )
        fidl_definitions.append(
            f"\n// STUB {syscall_number}\nstrict A{syscall_number:04}{camel_case_name}() -> () error int64;\n"
        )
        syscall_count += 1
        if syscall_count != syscall_number + 1:
            print("syscall_count is {}".format(syscall_count))
            print("syscall_number is {}".format(syscall_number))
    return "".join(fidl_definitions)


def write_fidls(fidl_definitions, output_file):
    with open(output_file, "w") as f:
        f.write(fidl_definitions)


def main():
    parser = argparse.ArgumentParser(
        description="Convert SYSCALL_DEFINE strings in Linux source code to FIDL definitions."
    )
    parser.add_argument(
        "input_path",
        help="Path to the input file or directory containing SYSCALL_DEFINE macros",
    )
    parser.add_argument(
        "tbl_file",
        help="Path to the syscall tbl file specifying the order of syscalls",
    )
    parser.add_argument("output_file", help="Path to the output FIDL file")
    args = parser.parse_args()

    syscall_info = parse_syscall_tbl(args.tbl_file)
    #syscalls = find_syscalls(args.input_path)
    fidl_definitions = generate_fidls(syscall_info)
    write_fidls(fidl_definitions, args.output_file)


if __name__ == "__main__":
    main()
