#!/usr/bin/env fuchsia-vendored-python

# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for android_boot_image.py."""

import pathlib
import pkgutil
import tempfile
import unittest

import android_boot_image
from android_boot_image import AndroidBootImage, ChunkType


def test_image() -> bytes:
    """Returns the test_boot_image.bin contents"""
    # We're including the test data in our build rule `sources` component, which ends
    # up including it in the resulting .pyz file, so we can use pkgutil to find and
    # read it.
    #
    # One annoying result of this is that running this binary manually from source
    # does not work - you have to build it and use `fx test` to run the resulting .pyz.
    image = pkgutil.get_data("android_boot_image_test", "test_boot_image.bin")
    if not image:
        raise FileNotFoundError("Failed to load testdata - run with `fx test`")
    return image


def create_boot_image(chunks: dict[ChunkType, bytes]) -> AndroidBootImage:
    """Helper to create a boot image with the given chunk contents."""
    # Ideally this would generate a fresh image via `mkbootimg` but it's difficult
    # to access that script from this test, so modify our test image instead.
    #
    # This assumes that boot image modification is working properly, so we need
    # at least a few tests that do not use this function but instead use the
    # generated test image directly.
    image = android_boot_image.load_images(test_image())[0]
    for chunk_type in ChunkType:
        image.replace_chunk(chunk_type, chunks.get(chunk_type, b""))
    return image


class BootImageTests(unittest.TestCase):
    def test_load_boot_image(self) -> None:
        """Loads a boot image and verifies the chunk contents."""
        images = android_boot_image.load_images(test_image())
        self.assertEqual(len(images), 1)
        image = images[0]

        # Expected contents taken from `generate_testdata.py`.
        self.assertEqual(
            image.get_chunk_data(ChunkType.KERNEL), b"test kernel contents"
        )
        self.assertEqual(
            image.get_chunk_data(ChunkType.RAMDISK), b"test ramdisk contents"
        )
        self.assertEqual(image.get_chunk_data(ChunkType.SECOND), b"")
        self.assertEqual(image.get_chunk_data(ChunkType.RECOVERY_DTBO), b"")
        self.assertEqual(
            image.get_chunk_data(ChunkType.DTB), b"test dtb contents"
        )

        # Header and chunks should each take up 1 4096-byte page.
        self.assertEqual(image.total_size, 4096 * 4)

    def test_replace_chunk(self) -> None:
        """Replaces a chunk with new data."""
        image = android_boot_image.load_images(test_image())[0]

        image.replace_chunk(ChunkType.RAMDISK, b"new ramdisk")

        self.assertEqual(
            image.get_chunk_data(ChunkType.RAMDISK), b"new ramdisk"
        )

    def test_load_two_boot_images(self) -> None:
        """Loads two concatenated boot images."""
        image1 = create_boot_image(
            {ChunkType.KERNEL: b"kernel1", ChunkType.SECOND: b"second1"}
        )
        image2 = create_boot_image(
            {ChunkType.KERNEL: b"kernel2", ChunkType.DTB: b"dtb2"}
        )
        combined_contents = image1.image + image2.image

        images = android_boot_image.load_images(combined_contents)
        self.assertEqual(len(images), 2)

        self.assertEqual(images[0].get_chunk_data(ChunkType.KERNEL), b"kernel1")
        self.assertEqual(images[0].get_chunk_data(ChunkType.RAMDISK), b"")
        self.assertEqual(images[0].get_chunk_data(ChunkType.SECOND), b"second1")
        self.assertEqual(images[0].get_chunk_data(ChunkType.RECOVERY_DTBO), b"")
        self.assertEqual(images[0].get_chunk_data(ChunkType.DTB), b"")

        self.assertEqual(images[1].get_chunk_data(ChunkType.KERNEL), b"kernel2")
        self.assertEqual(images[1].get_chunk_data(ChunkType.RAMDISK), b"")
        self.assertEqual(images[1].get_chunk_data(ChunkType.SECOND), b"")
        self.assertEqual(images[1].get_chunk_data(ChunkType.RECOVERY_DTBO), b"")
        self.assertEqual(images[1].get_chunk_data(ChunkType.DTB), b"dtb2")

    def test_commandline_dump_ramdisk(self) -> None:
        """Tests the "dump ramdisk" commandline option."""
        with tempfile.TemporaryDirectory() as temp_dir_str:
            temp_dir = pathlib.Path(temp_dir_str)
            input_path = temp_dir / "boot.img"
            output_path = temp_dir / "ramdisk.img"

            input_path.write_bytes(
                create_boot_image(
                    {
                        ChunkType.KERNEL: b"test_kernel",
                        ChunkType.RAMDISK: b"test_ramdisk",
                    }
                ).image
            )

            android_boot_image.main(
                [str(input_path), "--dump_ramdisk", str(output_path)]
            )

            self.assertEqual(output_path.read_bytes(), b"test_ramdisk")

    def test_commandline_dump_ramdisk_multiple_images(self) -> None:
        """Tests the "dump ramdisk" commandline option on multiple images."""
        with tempfile.TemporaryDirectory() as temp_dir_str:
            temp_dir = pathlib.Path(temp_dir_str)
            input_path = temp_dir / "boot.img"
            output_path = temp_dir / "ramdisk.img"

            input_path.write_bytes(
                create_boot_image({ChunkType.RAMDISK: b"ramdisk0"}).image
                + create_boot_image({ChunkType.RAMDISK: b"ramdisk1"}).image
            )

            # Dump ramdisk from the first image.
            android_boot_image.main(
                [
                    str(input_path),
                    "--dump_ramdisk",
                    str(output_path),
                    "--select_image",
                    "0",
                ]
            )
            self.assertEqual(output_path.read_bytes(), b"ramdisk0")

            # Dump ramdisk from the second image.
            android_boot_image.main(
                [
                    str(input_path),
                    "--dump_ramdisk",
                    str(output_path),
                    "--select_image",
                    "1",
                ]
            )
            self.assertEqual(output_path.read_bytes(), b"ramdisk1")

    def test_commandline_replace_ramdisk(self) -> None:
        """Tests the "replace ramdisk" commandline option."""
        with tempfile.TemporaryDirectory() as temp_dir_str:
            temp_dir = pathlib.Path(temp_dir_str)
            input_path = temp_dir / "boot.img"
            ramdisk_path = temp_dir / "ramdisk.img"

            input_path.write_bytes(
                create_boot_image(
                    {
                        ChunkType.KERNEL: b"kernel",
                        ChunkType.RAMDISK: b"ramdisk",
                        ChunkType.DTB: b"DTB",
                    }
                ).image
            )
            ramdisk_path.write_bytes(b"new ramdisk")

            android_boot_image.main(
                [str(input_path), "--replace_ramdisk", str(ramdisk_path)]
            )

            # Re-load the file and make sure only the ramdisk was modified.
            images = android_boot_image.load_images(input_path.read_bytes())
            self.assertEqual(len(images), 1)
            image = images[0]

            self.assertEqual(image.get_chunk_data(ChunkType.KERNEL), b"kernel")
            self.assertEqual(
                image.get_chunk_data(ChunkType.RAMDISK), b"new ramdisk"
            )
            self.assertEqual(image.get_chunk_data(ChunkType.SECOND), b"")
            self.assertEqual(image.get_chunk_data(ChunkType.RECOVERY_DTBO), b"")
            self.assertEqual(image.get_chunk_data(ChunkType.DTB), b"DTB")

    def test_commandline_replace_ramdisk_multiple_images(self) -> None:
        """Tests the "replace ramdisk" commandline option on multiple images."""
        with tempfile.TemporaryDirectory() as temp_dir_str:
            temp_dir = pathlib.Path(temp_dir_str)
            input_path = temp_dir / "boot.img"
            ramdisk_path = temp_dir / "ramdisk0.img"

            input_path.write_bytes(
                create_boot_image({ChunkType.RAMDISK: b"ramdisk0"}).image
                + create_boot_image({ChunkType.RAMDISK: b"ramdisk1"}).image
            )

            # Replace ramdisk on the first image.
            ramdisk_path.write_bytes(b"new ramdisk 0")
            android_boot_image.main(
                [
                    str(input_path),
                    "--replace_ramdisk",
                    str(ramdisk_path),
                    "--select_image",
                    "0",
                ]
            )

            # Replace ramdisk on the second image.
            ramdisk_path.write_bytes(b"new ramdisk 1")
            android_boot_image.main(
                [
                    str(input_path),
                    "--replace_ramdisk",
                    str(ramdisk_path),
                    "--select_image",
                    "1",
                ]
            )

            # Re-load the file and make sure it has the expected changes.
            images = android_boot_image.load_images(input_path.read_bytes())
            self.assertEqual(len(images), 2)

            self.assertEqual(images[0].get_chunk_data(ChunkType.KERNEL), b"")
            self.assertEqual(
                images[0].get_chunk_data(ChunkType.RAMDISK), b"new ramdisk 0"
            )
            self.assertEqual(images[0].get_chunk_data(ChunkType.SECOND), b"")
            self.assertEqual(
                images[0].get_chunk_data(ChunkType.RECOVERY_DTBO), b""
            )
            self.assertEqual(images[0].get_chunk_data(ChunkType.DTB), b"")

            self.assertEqual(images[1].get_chunk_data(ChunkType.KERNEL), b"")
            self.assertEqual(
                images[1].get_chunk_data(ChunkType.RAMDISK), b"new ramdisk 1"
            )
            self.assertEqual(images[1].get_chunk_data(ChunkType.SECOND), b"")
            self.assertEqual(
                images[1].get_chunk_data(ChunkType.RECOVERY_DTBO), b""
            )
            self.assertEqual(images[1].get_chunk_data(ChunkType.DTB), b"")

    def test_commandline_multiple_images_required_selection(self) -> None:
        """Interacting with multiple images requires explicit selection."""
        with tempfile.TemporaryDirectory() as temp_dir_str:
            temp_dir = pathlib.Path(temp_dir_str)
            input_path = temp_dir / "boot.img"
            output_path = temp_dir / "ramdisk.img"

            input_path.write_bytes(
                create_boot_image({ChunkType.RAMDISK: b"ramdisk0"}).image
                + create_boot_image({ChunkType.RAMDISK: b"ramdisk1"}).image
            )

            with self.assertRaises(ValueError) as error:
                android_boot_image.main(
                    [str(input_path), "--dump_ramdisk", str(output_path)]
                )
                self.assertIn("you must select", str(error))

            with self.assertRaises(ValueError) as error:
                android_boot_image.main(
                    [str(input_path), "--replace_ramdisk", str(output_path)]
                )
                self.assertIn("you must select", str(error))


if __name__ == "__main__":
    unittest.main()
