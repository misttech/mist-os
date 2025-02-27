#!/usr/bin/env fuchsia-vendored-python

# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Android boot image utility.

This script contains some Fuchsia-specific logic, e.g. checking for concatenated boot
images in a single file.

Upstream tooling with more standard functionality can be found at
https://android.googlesource.com/platform/system/tools/mkbootimg/+/refs/heads/main.

Example usage:

1. Dump the ramdisk:
$ android_boot_image.py boot.img --dump_ramdisk ramdisk.img

2. Replace the ramdisk:
$ android_boot_image.py boot.img --replace_ramdisk new_ramdisk.img
"""

import argparse
import dataclasses
import enum
import pathlib
import struct


class ChunkType(enum.Enum):
    """The Android boot image chunk types."""

    KERNEL = "kernel"
    RAMDISK = "ramdisk"
    SECOND = "second"
    RECOVERY_DTBO = "recovery DTBO"
    DTB = "DTB"


@dataclasses.dataclass
class ImageChunk:
    """A single data chunk in an Android boot image."""

    # Chunk type.
    chunk_type: ChunkType
    # Chunk size (without padding).
    size: int
    # Chunk offset in the image.
    offset: int
    # Offset to the chunk size U32 value in the header (without padding).
    size_offset: int


class AndroidBootImage:
    """An Android boot image."""

    # First 8 bytes for all versions.
    MAGIC = b"ANDROID!"
    # The header version is a little-endian U32 at this offset.
    VERSION_OFFSET = 40

    def __init__(self, image: bytes):
        """Creates an AndroidBootImage from the raw bytes"""
        self._load(image)

    def _load(self, image: bytes) -> None:
        """Loads the given image.

        Raises an exception if the image looks wrong or isn't a supported version.
        """
        if not image.startswith(self.MAGIC):
            raise ValueError("Not an Android boot image")

        # Currently we only ever produce v2 boot images, so for simplicity that's all
        # we support here, but if we want other versions it should just be a matter
        # of specifying the correct chunks below.
        self.version = struct.unpack_from("<I", image, self.VERSION_OFFSET)[0]
        if self.version != 2:
            raise NotImplementedError(
                f"Only v2 is supported (v={self.version})"
            )

        self.page_size = struct.unpack_from("<I", image, 36)[0]

        # v2 image chunks appear in this order and are all padded to page alignment.
        chunk_type_and_offset = (
            (ChunkType.KERNEL, 8),
            (ChunkType.RAMDISK, 16),
            (ChunkType.SECOND, 24),
            (ChunkType.RECOVERY_DTBO, 1632),
            (ChunkType.DTB, 1648),
        )

        # Unpack the chunks, starting at page 1 (after the header).
        self.chunks = []
        self.total_size = self.page_size
        for chunk_type, size_offset in chunk_type_and_offset:
            size = struct.unpack_from("<I", image, size_offset)[0]
            self.chunks.append(
                ImageChunk(
                    chunk_type=chunk_type,
                    size=size,
                    offset=self.total_size,
                    size_offset=size_offset,
                )
            )
            self.total_size += self._align(size)

        self.image = image[: self.total_size]

    def _align(self, offset: int) -> int:
        """Rounds the given offset up to the page alignment."""
        return (
            (offset + (self.page_size - 1)) // self.page_size * self.page_size
        )

    def get_chunk(self, chunk_type: ChunkType) -> ImageChunk:
        """Returns the requested chunk, or raises an exception."""
        return [c for c in self.chunks if c.chunk_type == chunk_type][0]

    def get_chunk_data(self, chunk_type: ChunkType) -> bytes:
        """Returns a copy of the given chunk's data without padding."""
        chunk = self.get_chunk(chunk_type)
        return self.image[chunk.offset : chunk.offset + chunk.size]

    def replace_chunk(self, chunk_type: ChunkType, new_contents: bytes) -> None:
        """Replaces the given chunk with the new contents."""
        padding_size = self._align(len(new_contents)) - len(new_contents)
        chunk = self.get_chunk(chunk_type)

        # The recovery DTBO is unique in that the header also tracks its data offset.
        # We currently don't use a recovery DTBO and it adds a bit more complexity,
        # so for now just double-check that it doesn't exist so we can ignore it.
        if self.get_chunk(ChunkType.RECOVERY_DTBO).size != 0:
            raise NotImplementedError(
                "Replacing chunks not supported when recovery DTBO exists"
            )

        # Create the new image, replacing the chunk and its size in the header.
        new_image = (
            # Header up until the size field.
            self.image[: chunk.size_offset]
            # New chunk size.
            + struct.pack("<I", len(new_contents))
            # Rest of the header and data until the chunk start.
            + self.image[chunk.size_offset + 4 : chunk.offset]
            # New chunk data.
            + new_contents
            # New chunk padding.
            + b"\x00" * padding_size
            # Rest of the chunks.
            + self.image[chunk.offset + self._align(chunk.size) :]
        )

        # Re-load to update our internal data for the new contents.
        self._load(new_image)


def load_images(contents: bytes) -> list[AndroidBootImage]:
    """Loads any number of consecutive boot images."""
    images = []
    while contents:
        # Extract the current image.
        image = AndroidBootImage(contents)
        images.append(image)
        # Advance contents to look for the next one.
        contents = contents[image.total_size :]
    return images


def _print_images_info(images: list[AndroidBootImage]) -> None:
    """Prints images summary to stdout."""
    single_image = len(images) == 1
    if not single_image:
        print(f"Found {len(images)} images")
    for i, image in enumerate(images):
        if not single_image:
            print(f"== Image {i} ==")
        print(f"Version: {image.version}")
        for chunk in image.chunks:
            print(f"{chunk.chunk_type.value} size: {chunk.size}")
        print(f"Total size including padding: {image.total_size}")


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    """Parses script arguments.

    argv can be specified for tests, or None to use the actual commandline.
    """
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )

    parser.add_argument(
        "file", type=pathlib.Path, help="Path to Android boot image"
    )

    # Currently this expect an int index, but we could add more power here e.g.
    # --select-image=kernel to auto-detect which one is the kernel boot image.
    parser.add_argument(
        "--select_image",
        type=int,
        help="If the file contains multiple boot images, which one to use (0-based)",
    )

    parser.add_argument(
        "--dump_ramdisk",
        type=pathlib.Path,
        help="Dump the ramdisk to this path",
    )

    parser.add_argument(
        "--replace_ramdisk",
        type=pathlib.Path,
        help="Replace the ramdisk with the image at this path",
    )

    return parser.parse_args(argv)


def _select_image(
    index: int | None, images: list[AndroidBootImage]
) -> tuple[int, AndroidBootImage]:
    """Returns the selected index and boot image.

    If there is only one boot image, index is allowed to be None. Otherwise index must
    select a valid boot image from the list.

    Raises an exception if the requested image is not available.
    """
    if not images:
        raise ValueError("No boot images were found")

    if index is None:
        if len(images) == 1:
            return 0, images[0]
        raise ValueError(
            f"{len(images)} images were found, you must select one"
        )

    return index, images[index]


def main(argv: list[str] | None = None) -> None:
    """Main entry point.

    argv can be specified for tests, or None to use the actual commandline.
    """
    args = _parse_args(argv)

    images = load_images(args.file.read_bytes())
    _print_images_info(images)

    # Dump first so that if we also replace the ramdisk, we dump the old one.
    if args.dump_ramdisk:
        _, image = _select_image(args.select_image, images)
        args.dump_ramdisk.write_bytes(image.get_chunk_data(ChunkType.RAMDISK))
        print(f"Wrote ramdisk to {args.dump_ramdisk}")
    if args.replace_ramdisk:
        index, image = _select_image(args.select_image, images)
        image.replace_chunk(
            ChunkType.RAMDISK, args.replace_ramdisk.read_bytes()
        )
        images[index] = image
        args.file.write_bytes(b"".join(i.image for i in images))
        print(f"Replaced {args.file} ramdisk with {args.replace_ramdisk}")


if __name__ == "__main__":
    main()
