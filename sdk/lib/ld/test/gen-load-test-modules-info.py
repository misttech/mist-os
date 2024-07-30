#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--output",
        type=argparse.FileType("w"),
        help="Output file",
        required=True,
    )
    parser.add_argument(
        "--depfile",
        type=argparse.FileType("w"),
        required=True,
    )
    parser.add_argument(
        "--module-list-file",
        type=argparse.FileType("r"),
        help="JSON file containing list of ELF files",
        required=True,
    )
    parser.add_argument("readelf", help="llvm-readelf binary", nargs=1)
    args = parser.parse_args()

    # Map the build directory file names to the runtime file names.
    elf_files = dict(
        [
            (entry, os.path.basename(entry))
            if isinstance(entry, str)
            else (entry["source"], os.path.basename(entry["destination"]))
            for entry in json.load(args.module_list_file)
        ]
    )

    args.module_list_file.close()
    args.depfile.write(f"{args.output.name}: {' '.join(elf_files)}\n")
    args.depfile.close()

    data = json.loads(
        subprocess.check_output(
            [
                args.readelf[0],
                "--elf-output-style=JSON",
                "--program-headers",
                "--notes",
            ]
            + list(elf_files.keys())
        )
    )

    def process_file(file):
        source = file["FileSummary"]["File"]
        dest = elf_files[source]

        def gen_notes():
            if "Notes" in file:
                # Old schema.
                for note in file["Notes"]:
                    yield note["NoteSection"]["Note"]
            else:
                # New schema.
                for notesection in file["NoteSections"]:
                    notesection = notesection["NoteSection"]
                    for note in notesection["Notes"]:
                        yield note

        build_id = None
        for note in gen_notes():
            build_id = note.get("Build ID")
            if build_id is not None:
                break
        assert build_id is not None, f"no Build ID in {file}"

        args.output.write(
            'TestModule{"%(dest)s", "%(build_id)s", { // %(source)s\n'
            % {
                "dest": dest,
                "build_id": build_id,
                "source": source,
            }
        )

        for phdr in file["ProgramHeaders"]:
            phdr = phdr["ProgramHeader"]
            if phdr["Type"]["Name"] == "PT_LOAD":
                phdr = {
                    "vaddr": phdr["VirtualAddress"],
                    "memsz": phdr["MemSize"],
                    "flags": phdr["Flags"]["Value"],
                }
                args.output.write(
                    "           {.flags=%(flags)#x, .vaddr=%(vaddr)#x, .memsz=%(memsz)#x},\n"
                    % phdr
                )

        args.output.write("           }},\n")

    for file in data:
        process_file(file)

    return 0


if __name__ == "__main__":
    sys.exit(main())
