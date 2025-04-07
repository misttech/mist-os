# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Process the debug_symbols.json build API module in various ways."""

import json
import os
import typing as T
from pathlib import Path


def extract_gnu_build_id(elf_file: str | Path) -> str:
    """Extracts the GNU build ID from an ELF64 file.

    Args:
        elf_file: Path to input file.
    Returns:
        The build-id value has an hexadecimal string, or
        an empty string on failure (e.g. not an ELF file,
        or no .note.gnu.build-id section in it).
    """
    try:
        # See https://en.wikipedia.org/wiki/Executable_and_Linkable_Format
        with open(elf_file, "rb") as f:
            # Read ELF header
            e_ident = f.read(16)
            if e_ident[:4] != b"\x7fELF":
                return ""  # Not an ELF file

            def read(count: int) -> int:
                return int.from_bytes(f.read(count), "little")

            f.seek(40)  # e_shoff location.
            e_shoff = read(8)
            if e_shoff == 0:
                return ""  # no section headers.

            f.seek(58)  # read e_shentsize, e_shnum and e_shstrndx
            e_shentsize, e_shnum, e_shstrndx = read(2), read(2), read(2)

            f.seek(
                e_shoff + e_shentsize * e_shstrndx + 24
            )  # sh_offset in string table Elf64_shdr entry
            shstrtab_offset = read(8)

            f.seek(e_shoff)
            for i in range(e_shnum):
                f.seek(e_shoff + i * e_shentsize)
                sh_name = read(4)
                f.read(4 + 8 + 8)  # skip sh_flags and sh_addr
                sh_offset = read(8)
                sh_size = read(8)

                f.seek(shstrtab_offset + sh_name)
                name = b""
                while True:
                    byte = f.read(1)
                    if byte == b"\x00":
                        break
                    name += byte

                if name == b".note.gnu.build-id":
                    f.seek(sh_offset + 16)  # note description offset.
                    return f.read(sh_size - 16).hex()
    except Exception:
        # Ignore missing file, unreadable file, or truncated file errors.
        pass
    return ""


class DebugSymbolsManifestParser(object):
    """Parse the debug_symbols.json build API module and expand its
    included manifests to generate a flattened list of entries.

    See comments for //:debug_symbols target for schema description.

    Usage is:
        1) Create instance.

        2) Call parse_manifest_file(). This will raise ValueError
           if the input is malformed, if an include cycle is
           detected.

        3) Use the |entries| property to get the flatenned list of entries,
           with duplicates removed.

        4) Use the |extra_input_files| property to get the files read by the
           parser due to manifest includes or when reading the build-ID values.
    """

    def __init__(self, build_dir: None | Path) -> None:
        """Create instance.

        Args:
           build_dir: Optional build directory Path. For entries that have a
              "manifest" key, the value will be a path relative to this value.
              Default value is the current directory.
        """
        self._build_dir = build_dir if build_dir else Path(os.getcwd())
        self._visited_paths: T.Set[Path] = set()
        self._visited_stack: T.List[Path] = []
        self._entries: T.List[T.Dict[str, str]] = []
        self._resolve_build_id = False
        self._get_build_id: T.Callable[[str | Path], str] = extract_gnu_build_id
        self._elfinfo: T.Any = None

    def enable_build_id_resolution(self) -> None:
        """Ensure the build-id value is computed if not present in an entry."""
        self._resolve_build_id = True

    def set_build_id_callback_for_test(
        self, get_build_id: T.Callable[[str | Path], str]
    ) -> None:
        self._get_build_id = get_build_id

    @property
    def entries(self) -> T.List[T.Dict[str, str]]:
        """The flattened list of parsed entries, each one describing a single ELF file."""
        return self._entries

    @property
    def extra_input_files(self) -> T.Set[Path]:
        """The set of extra input files read by the parsing functions.

        These maybe be used to write implicit inputs in a Ninja depfile
        for a script invoked through a GN action, to ensure proper
        incremental builds.
        """
        return self._visited_paths

    def parse_manifest_file(self, manifest_path: Path) -> None:
        """Parse a given debug_symbols.json manifest.

        Args:
            manifest_path: Path to input manifest.
        Raises:
            ValueError if the manifest, or one of its includes is
            malformed, or if there is a cycle in the include chain.
        """
        if not manifest_path.exists():
            raise ValueError(f"Missing manifest file at {manifest_path}")

        with manifest_path.open("rt") as f:
            manifest_json = json.load(f)

        self._parse_manifest(manifest_json, manifest_path)

    def _parse_entry(self, entry: T.Any, manifest_path: Path) -> None:
        debug = entry.get("debug")
        if debug:
            if self._resolve_build_id and "elf_build_id" not in entry:
                build_id = ""

                # First, look at the debug value if it looks like ../.build-id/xx/yyyyyyyy.debug
                pos = debug.find(".build-id/")
                if pos >= 0:
                    pos += len(".build-id/")
                    rest = debug[pos:]
                    if (
                        len(rest) > 3
                        and rest[2] == "/"
                        and rest.endswith(".debug")
                    ):
                        build_id = rest[0:2] + rest[3:-6]

                if not build_id:
                    # Second, look for an elf_build_id_file path.
                    build_id_file = entry.get("elf_build_id_file")
                    if build_id_file:
                        build_id_file_path = self._build_dir / build_id_file
                        self._visited_paths.add(build_id_file_path)
                        build_id = build_id_file_path.read_text().strip()
                        # It is possible for certain files to have an empty
                        # build_id file, for example, see
                        # /zircon/kernel/lib/libc/string/arch/x86:_hermetic_code_blob.memcpy_movsb.executable(//zircon/kernel:kernel_x64)

                if not build_id:
                    # As a fallback, try to extract the build id directly from the file.
                    debug_path = self._build_dir / debug
                    self._visited_paths.add(debug_path)
                    build_id = self._get_build_id(debug_path)
                    if not build_id:
                        # It is possible for unstripped files to not have a GNU .build-id
                        # value, for example Go host binaries.
                        return

                entry["elf_build_id"] = build_id

            self._entries.append(entry)
            return

        sub_manifest = entry.get("manifest")
        if not sub_manifest:
            raise ValueError(f"Malformed entry in {manifest_path}: {entry}")

        sub_manifest_path = (self._build_dir / sub_manifest).resolve()
        if not sub_manifest_path.exists():
            raise ValueError(
                f"Missing include path in {manifest_path}: {sub_manifest}"
            )

        # Check for cycles in manifest include chain. Raise exception if one
        # is detected.
        for n, visited in enumerate(self._visited_stack):
            if visited == sub_manifest_path:
                error_msg = "Recursive manifest includes:\n"
                for visited in self._visited_stack[n:]:
                    error_msg += f"  {visited} -->\n"
                error_msg += f"  {sub_manifest_path}"
                raise ValueError(error_msg)

        self._visited_paths.add(sub_manifest_path)
        self._visited_stack.append(sub_manifest_path)

        self.parse_manifest_file(sub_manifest_path)

        self._visited_stack = self._visited_stack[:-1]

    def _parse_manifest(
        self, manifest_json: T.Any, manifest_path: Path
    ) -> None:
        if not isinstance(manifest_json, list):
            raise ValueError(
                f"Malformed manifest at {manifest_path}: expected list, got {type(manifest_json)}"
            )

        for entry in manifest_json:
            self._parse_entry(entry, manifest_path)
