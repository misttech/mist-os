#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import struct
import sys
import tempfile
import typing as T
import unittest
from pathlib import Path

sys.path.insert(0, Path(__file__).parent)
from debug_symbols import DebugSymbolsManifestParser, extract_gnu_build_id


def write_json(path: Path, content: T.Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wt") as f:
        json.dump(content, f)


def generate_elf_with_build_id(build_id: bytes) -> bytes:
    """Generate a tiny ELF64 file that only contains a GNU .build-id note

    Args:
        build_id: The .build-id value as a bytes array.
    Returns:
        Content of the ELF file as a bytes array.
    """
    # This works in two passes. In the first pass, no data is generated, but write
    # positions are computed based on formatted ELF structures being appended,
    # the non-format values passed to add_struct() being ignored. This allows
    # computing all important offsets.
    #
    # On the second pass, the struct bytes are actually written to the output
    # and include the proper offset values computed in the first pass.

    def roundup8(value: int) -> int:
        return (value + 7) & -8

    class Writer(object):
        def __init__(self, enable_output: bool) -> None:
            self.offset = 0
            self.output = b""
            self.enable_output = enable_output

        def add_bytes(self, b: bytes) -> int:
            """Append a slice of bytes, then pad with zeroes for 8 alignment."""
            write_size = len(b)
            if self.enable_output:
                self.output += b
                padding = -write_size & 7
                self.output += b"\0" * padding
            self.offset = roundup8(self.offset + write_size)
            return write_size

        def add_struct(self, fmt: str, *args) -> int:
            """Append a formatted struct, then pad with zeroes for 8 alignment."""
            if not self.enable_output:
                write_size = struct.calcsize(fmt)
                self.offset = roundup8(self.offset + write_size)
                return write_size

            return self.add_bytes(struct.pack(fmt, *args))

    program_header_offset = 0
    program_header_size = 0
    program_header_count = 0

    section_header_offset = 0
    section_header_size = 0
    section_header_count = 0

    string_table_offset = 0
    note_section_offset = 0

    for enable_output in (False, True):
        writer = Writer(enable_output)

        # ELF header - https://en.wikipedia.org/wiki/Executable_and_Linkable_Format#ELF_header
        writer.add_struct(
            "<16sHHIQQQIHHHHHH",
            b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00",
            2,  # Type: ET_EXEC
            0x3E,  # Machine: AMD x86-64
            1,  # Version
            0,  # Entry point
            program_header_offset,  # Program header offset
            section_header_offset,  # Section header offset
            0,  # Flags
            0x40,  # ELF header size
            program_header_size,  # Program header entry size
            program_header_count,  # Program header entry count
            section_header_size,  # Section header entry size
            section_header_count,  # Section header entry count
            1,
        )  # Section header string table index

        # Program header (LOAD segment) - https://en.wikipedia.org/wiki/Executable_and_Linkable_Format#Program_header
        program_header_offset = writer.offset
        program_header_count = 1
        program_header_size = writer.add_struct(
            "<IIQQQQQQ",
            1,  # p_type: PT_LOAD
            5,  # p_flags: PF_R | PF_X
            0,  # p_offset
            0,  # p_vaddr
            0,  # p_paddr
            0x1000,  # p_filesz
            0x1000,  # p_memsz
            0x1000,
        )  # p_align
        assert program_header_size == 0x38

        # Note section data - https://docs.oracle.com/cd/E23824_01/html/819-0690/chapter6-18048.html
        # Surprisingly, there is no official spec for the content of this note. From various sources:
        # There must be a single section named .note.gnu.build-id, with a single note with vendor
        # name "GNU\x00", and type NT_GNU_BUILD_ID.
        note_section_offset = writer.offset
        note_name = b"GNU\x00"
        note_desc = build_id
        note_header = struct.pack(
            "<III", len(note_name), len(note_desc), 3
        )  # NT_GNU_BUILD_ID
        note_section_data = note_header + note_name + note_desc
        writer.add_bytes(note_section_data)

        # String table data - https://refspecs.linuxbase.org/elf/gabi4+/ch4.strtab.html
        str1_index = 1  # First symbol starts after initial 0 byte.
        string_table = b"\x00.shstrtab\x00"
        str2_index = len(string_table)
        string_table += b".note.gnu.build-id\x00"

        string_table_offset = writer.offset
        writer.add_bytes(string_table)

        # Section headers - https://en.wikipedia.org/wiki/Executable_and_Linkable_Format#Section_header
        section_header_offset = writer.offset
        section_header_count = 2
        section_header_size = writer.add_struct(
            "<IIQQQQIIQQ",
            str2_index,  # sh_name
            7,  # sh_type: SHT_NOTE
            0,  # sh_flags
            0,  # sh_addr
            note_section_offset,  # sh_offset
            len(note_section_data),  # sh_size
            0,  # sh_link
            0,  # sh_info
            8,  # sh_addralign
            0,
        )  # sh_entsize
        assert section_header_size == 0x40

        writer.add_struct(
            "<IIQQQQIIQQ",
            str1_index,  # sh_name
            7,  # sh_type: SHT_NOTE
            0,  # sh_flags
            0,  # sh_addr
            string_table_offset,  # sh_offset
            len(string_table),  # sh_size
            0,  # sh_link
            0,  # sh_info
            8,  # sh_addralign
            0,
        )  # sh_entsize

    return writer.output


# A constant .build-id value used by several tests.
TINY_ELF_BUILD_ID_VALUE = bytes.fromhex(
    "0123456789012345678901234567890123456789"
)


class ExtractGnuBuildIdTest(unittest.TestCase):
    def setUp(self) -> None:
        self._td = tempfile.TemporaryDirectory()
        self._root = Path(self._td.name)
        self._elf = self._root / "elf_file"

    def tearDown(self) -> None:
        self._td.cleanup()

    def _write_elf_with_build_id(self, build_id: bytes) -> None:
        self._elf.write_bytes(generate_elf_with_build_id(build_id))

    def test_1(self) -> None:
        self._write_elf_with_build_id(TINY_ELF_BUILD_ID_VALUE)
        self.assertEqual(
            extract_gnu_build_id(self._elf), TINY_ELF_BUILD_ID_VALUE.hex()
        )

        self._write_elf_with_build_id(b"\0")
        self.assertEqual(extract_gnu_build_id(self._elf), "00")

        self._write_elf_with_build_id(b"\xde\xad\xbe\xef")
        self.assertEqual(extract_gnu_build_id(self._elf), "deadbeef")


class DebugSymbolsManifestParserTest(unittest.TestCase):
    def setUp(self) -> None:
        self._td = tempfile.TemporaryDirectory()
        self._root = Path(self._td.name)

    def tearDown(self) -> None:
        self._td.cleanup()

    def _write_manifest(self, relpath: str, content: T.Any) -> Path:
        path = self._root / relpath
        write_json(path, content)
        return path

    def test_single_manifest(self) -> None:
        manifest = [
            {
                "label": "//src/lib/foo:lib",
                "debug": "obj/lib/foo/libfoo.so.unstripped",
                "elf_build_id_file": "obj/lib/foo/libfoo.so.elf_build_id",
                "dest_path": "lib/libfoo.so",
                "os": "fuchsia",
                "cpu": "arm64",
                "breakpad": "obj/lib/foo/libfoo.so.sym",
            }
        ]
        manifest_path = self._write_manifest("debug_symbols.json", manifest)

        parser = DebugSymbolsManifestParser(self._root)
        parser.parse_manifest_file(manifest_path)

        self.assertListEqual(parser.entries, manifest)
        self.assertSetEqual(parser.extra_input_files, set())

    def test_included_manifest(self) -> None:
        sub_manifest = [
            {
                "debug": "obj/prebuilt/packages/extracted/foo/.build-id/01/23456789.debug",
                "elf_build_id": "0123456789",
            }
        ]
        sub_relpath = "obj/prebuilt/packages/debug_symbols/foo/manifest.json"

        sub_path = self._write_manifest(sub_relpath, sub_manifest)

        manifest_path = self._write_manifest(
            "debug_symbols.json",
            [
                {
                    "label": "//prebuilt/packages/foo",
                    "manifest": sub_relpath,
                }
            ],
        )

        parser = DebugSymbolsManifestParser(self._root)
        parser.parse_manifest_file(manifest_path)

        self.assertListEqual(parser.entries, sub_manifest)
        self.assertSetEqual(parser.extra_input_files, {sub_path})

    def test_recursive_includes(self) -> None:
        top_path = self._write_manifest(
            "debug_symbols.json",
            [
                {
                    "debug": "top/a/debug_file",
                },
                {
                    "manifest": "middle/debug_symbols.json",
                },
                {
                    "debug": "top/b/debug_file",
                },
            ],
        )

        middle_path = self._write_manifest(
            "middle/debug_symbols.json",
            [
                {
                    "debug": "middle/c/debug_file",
                },
                {
                    "manifest": "bottom/debug_symbols.json",
                },
                {
                    "debug": "middle/d/debug_file",
                },
            ],
        )

        bottom_path = self._write_manifest(
            "bottom/debug_symbols.json", [{"debug": "bottom/e/debug_file"}]
        )

        parser = DebugSymbolsManifestParser(self._root)
        parser.parse_manifest_file(top_path)

        self.assertListEqual(
            parser.entries,
            [
                {
                    "debug": "top/a/debug_file",
                },
                {
                    "debug": "middle/c/debug_file",
                },
                {
                    "debug": "bottom/e/debug_file",
                },
                {
                    "debug": "middle/d/debug_file",
                },
                {
                    "debug": "top/b/debug_file",
                },
            ],
        )

        self.assertSetEqual(
            parser.extra_input_files, {middle_path, bottom_path}
        )

    def test_cycle_detection(self) -> None:
        # Write manifests such that:
        #
        #   a --> b --> c --> d
        #         ^           |
        #         |___________|
        #
        a_path = self._write_manifest(
            "a.manifest", [{"manifest": "b.manifest"}]
        )
        b_path = self._write_manifest(
            "b.manifest", [{"manifest": "c.manifest"}]
        )
        c_path = self._write_manifest(
            "c.manifest", [{"manifest": "d.manifest"}]
        )
        d_path = self._write_manifest(
            "d.manifest", [{"manifest": "b.manifest"}]
        )

        parser = DebugSymbolsManifestParser(self._root)

        with self.assertRaises(ValueError) as cm:
            parser.parse_manifest_file(a_path)

        self.assertEqual(
            str(cm.exception),
            f"""Recursive manifest includes:
  {b_path} -->
  {c_path} -->
  {d_path} -->
  {b_path}""",
        )

    def test_build_id_resolution(self) -> None:
        a_build_id_file = self._root / "a.build_id"
        a_build_id_file.write_text("build_id_for_a")

        manifest_path = self._write_manifest(
            "debug_symbols.json",
            [
                {
                    "debug": "a/libfoo.so.unstripped",
                    "elf_build_id_file": "a.build_id",
                },
                {"debug": "b/libbar.so.unstripped"},
                {
                    "debug": "c/.build-id/bu/ild_id_for_c.debug",
                },
                {
                    "debug": "d/libzoo.so.unstripped",
                    "elf_build_id": "build_id_for_d",
                },
            ],
        )

        b_file = self._root / "b/libbar.so.unstripped"
        b_file.parent.mkdir(parents=True)
        b_file.write_bytes(generate_elf_with_build_id(TINY_ELF_BUILD_ID_VALUE))

        parser = DebugSymbolsManifestParser(self._root)
        parser.enable_build_id_resolution()
        parser.parse_manifest_file(manifest_path)

        self.maxDiff = None

        self.assertListEqual(
            parser.entries,
            [
                {
                    "debug": "a/libfoo.so.unstripped",
                    "elf_build_id": "build_id_for_a",
                    "elf_build_id_file": "a.build_id",
                },
                {
                    "debug": "b/libbar.so.unstripped",
                    "elf_build_id": TINY_ELF_BUILD_ID_VALUE.hex(),
                },
                {
                    "debug": "c/.build-id/bu/ild_id_for_c.debug",
                    "elf_build_id": "build_id_for_c",
                },
                {
                    "debug": "d/libzoo.so.unstripped",
                    "elf_build_id": "build_id_for_d",
                },
            ],
        )

        self.assertSetEqual(
            parser.extra_input_files,
            {a_build_id_file, self._root / "b/libbar.so.unstripped"},
        )


if __name__ == "__main__":
    unittest.main()
