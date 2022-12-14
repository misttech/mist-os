#!/usr/bin/env fuchsia-vendored-python
#
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Script to create Fuchsia GPT images.

This script wraps the `sgdisk` utility to provide a simpler interface tailored
specifically for Fuchsia GPT images.
"""

import contextlib
import dataclasses
import os
import re
import subprocess
import tempfile
from typing import Iterator, List, Optional

# A regexp to capture GUIDs as reported by `sgdisk`.
# Example: 4B7A23A5-E9D1-456A-BACB-78B23D855324.
_GUID_RE = r"[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}"


@dataclasses.dataclass
class Partition():
    """GPT partition info."""
    # 1-based index in the GPT entry array.
    index: int
    name: str
    first_lba: int
    last_lba: int
    size: int
    type_guid: str
    unique_guid: str
    attr_flags: str


@contextlib.contextmanager
def gen_disk_image(source_gpt: str, image_size_mib: int) -> Iterator[str]:
    """ Generates empty disk image using the given source GPT."""
    print("Generating disk image")

    with tempfile.TemporaryDirectory() as temp_dir:
        image_path = os.path.join(temp_dir, "disk_image")

        with open(image_path, "wb") as file:
            file.truncate(image_size_mib * 1024 * 1024)

        subprocess.run(
            ["sgdisk", f"--load-backup={source_gpt}", image_path],
            capture_output=True,
            check=True)
        yield image_path
        print("Deleting disk image")


# This function is a bit brittle as it depends heavily on `sgdisk` output format
# which is not guaranteed to be stable. If it becomes unreliable we may either
# need to bring a fixed version of sgdisk into //prebuilts, or just implement
# GPT handling ourselves.
def get_partitions(disk_file: str) -> List[Partition]:
    """Returns a list of all Partitions."""
    # Find all the partition indices first.
    # Output looks like:
    # -----------------------------------------------------------------
    # Disk test_disk.img: 131072 sectors, 64.0 MiB
    # Sector size (logical): 512 bytes
    # Disk identifier (GUID): F1D74D6E-1DB4-4E46-8D2A-87AC93543ACA
    # Partition table holds up to 128 entries
    # Main partition table begins at sector 2 and ends at sector 33
    # First usable sector is 34, last usable sector is 131038
    # Partitions will be aligned on 2048-sector boundaries
    # Total free space is 126909 sectors (62.0 MiB)
    #
    # Number  Start (sector)    End (sector)  Size       Code  Name
    #    1            2048            4095   1024.0 KiB  8300
    #    2            4096            6143   1024.0 KiB  8300
    # -----------------------------------------------------------------
    # Here we just care about the index, which will be the first entry on lines
    # that start with a number.
    output = subprocess.run(
        ["sgdisk", disk_file, "--print"],
        text=True,
        capture_output=True,
        check=True).stdout
    indices = re.findall(r"^\s*(\d+)\s+", output, re.MULTILINE)

    partitions = []
    for index in indices:
        # Now fill in the full partition information. Output looks like:
        # -----------------------------------------------------------------
        # Partition GUID code: 0FC63DAF-8483-4772-8E79-3D69D8477DE4 (Linux filesystem)
        # Partition unique GUID: 4B7A23A5-E9D1-456A-BACB-78B23D855324
        # First sector: 2048 (at 1024.0 KiB)
        # Last sector: 4095 (at 2.0 MiB)
        # Partition size: 2048 sectors (1024.0 KiB)
        # Attribute flags: 0000000000000000
        # Partition name: 'foo'
        # -----------------------------------------------------------------
        # We could potentially query all partitions in a single command, but
        # the extra delay is negligible and this is simpler.
        output = subprocess.run(
            ["sgdisk", disk_file, "--info", index],
            text=True,
            capture_output=True,
            check=True).stdout

        def get_param(pattern):
            return re.search(f"^{pattern}.*?$", output, re.MULTILINE).group(1)

        partitions.append(
            Partition(
                index=int(index),
                name=get_param(r"Partition name: '(.*?)'"),
                first_lba=int(get_param(r"First sector: (\d+)")),
                last_lba=int(get_param(r"Last sector: (\d+)")),
                size=int(get_param(r"Partition size: (\d+) sectors")),
                type_guid=get_param(f"Partition GUID code: ({_GUID_RE})"),
                unique_guid=get_param(f"Partition unique GUID: ({_GUID_RE})"),
                attr_flags=get_param(r"Attribute flags: (\S*)")))

    return partitions


def get_partition(
        disk_file: str,
        *,
        part_name: Optional[str] = None,
        index: Optional[int] = None) -> Partition:
    """Returns a single Partition keyed by name and/or index.

    Raises ValueError if the requested partition couldn't be found.
    """
    if part_name is None and index is None:
        raise ValueError("Must specify either a name or index")

    def filter(part):
        return (part_name is None or part.name
                == part_name) and (index is None or part.index == index)

    partitions = get_partitions(disk_file)
    try:
        return next(p for p in partitions if filter(p))
    except StopIteration:
        raise ValueError(f"Failed to find partition {part_name}")


def get_part_num(disk_file: str, part_name: str) -> int:
    """Returns the numeric index of a given partition.

    Raises ValueError if the given partition name doesn't exist.
    """
    return get_partition(disk_file, part_name=part_name).index


def mib_to_lba(mib: int, blk_size: int) -> int:
    """Convert size in MB to lba"""
    return int(mib * 1024 * 1024 / blk_size)


def save_gpt(
        disk_file: str, image_size_lba: int, blk_size: int, *,
        flashing_gpt_file: str, primary_gpt_file: str, secondary_gpt_file: str,
        combined_gpt_file: str):
    """Extract and save GPT of a given disk

    The extracted image can be saved in provisioning/primary/secondary/combined
    format. The paths are specified by |flashing_gpt_file|, |primary_gpt_file|,
    |secondary_gpt_file| and |combined_gpt_file| respectively.
    """
    if flashing_gpt_file:
        subprocess.run(
            [
                "dd", f"if={disk_file}", f"of={flashing_gpt_file}",
                f"bs={blk_size}", "count=34"
            ],
            capture_output=True,
            check=True)
    if primary_gpt_file:
        subprocess.run(
            [
                "dd", f"if={disk_file}", f"of={primary_gpt_file}",
                f"bs={blk_size}", "skip=1", "count=33"
            ],
            capture_output=True,
            check=True)
    if secondary_gpt_file:
        subprocess.run(
            [
                "dd", f"if={disk_file}", f"of={secondary_gpt_file}",
                f"bs={blk_size}", f"skip={image_size_lba-33}", "count=33"
            ],
            capture_output=True,
            check=True)
    if combined_gpt_file:
        subprocess.run(
            [
                "dd", f"if={disk_file}", f"of={combined_gpt_file}",
                f"bs={blk_size}", "skip=1", "count=33"
            ],
            capture_output=True,
            check=True)
        subprocess.run(
            [
                "dd",
                f"if={disk_file}",
                f"of={combined_gpt_file}",
                f"bs={blk_size}",
                f"skip={image_size_lba-33}",
                "count=33",
                "oflag=append",
                "conv=notrunc",
            ],
            capture_output=True,
            check=True)


def delete_part(disk_file: str, part_num: int):
    """Delete a partition from the GPT"""
    subprocess.run(
        ["sgdisk", "--delete", f"{part_num}", disk_file],
        capture_output=True,
        check=True)


def delete_part_by_name(disk_file: str, name: str):
    """Delete a partition by name"""
    part_num = get_part_num(disk_file, name)
    delete_part(disk_file, part_num)


def new_part(disk_file: str, part_num: int, part: Partition):
    """Create a new partition"""
    # does not set attr_flags - this is fine for now as current Fuchsia devices
    # don't depend on it.
    subprocess.run(
        [
            "sgdisk", "-a", "1", "--new",
            f"{part_num}:{part.first_lba}:{part.last_lba}", disk_file
        ],
        capture_output=True,
        check=True)

    subprocess.run(
        ["sgdisk", "--change-name", f"{part_num}:{part.name}", disk_file],
        capture_output=True,
        check=True)
    subprocess.run(
        ["sgdisk", "--typecode", f"{part_num}:{part.type_guid}", disk_file],
        capture_output=True,
        check=True)

    # If we don't have a specific UUID to use, "R" creates a random one.
    guid = part.unique_guid or "R"
    subprocess.run(
        ["sgdisk", "--partition-guid", f"{part_num}:{guid}", disk_file],
        capture_output=True,
        check=True)


def gpt_rename_part(
        disk_image: str,
        old_part_name: str,
        new_part_name: str,
        new_part_type_guid: str = None):
    """Rename a partition in the GPT"""
    orig_part = get_partition(disk_image, part_name=old_part_name)
    new_partition = Partition(
        index=orig_part.index,
        name=new_part_name,
        first_lba=orig_part.first_lba,
        last_lba=orig_part.last_lba,
        size=orig_part.size,
        type_guid=new_part_type_guid or orig_part.type_guid,
        unique_guid="",  #generate new
        attr_flags=orig_part.attr_flags,
    )

    delete_part(disk_image, orig_part.index)
    new_part(disk_image, orig_part.index, new_partition)
