#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.ui.screenshot_image.py."""

import pathlib
import tempfile
import unittest

from parameterized import parameterized

# Disabling pylint to reduce verbosity for widely used trivial types
from honeydew.interfaces.affordances.ui.custom_types import (  # pylint: disable=g-importing-member
    Size,
)
from honeydew.typing.screenshot_image import (  # pylint: disable=g-importing-member
    ScreenshotImage,
)
from honeydew.typing.ui import Pixel  # pylint: disable=g-importing-member

RED = Pixel(255, 0, 0)
GREEN = Pixel(0, 255, 0)
BLUE = Pixel(0, 0, 255)
BLACK = Pixel(0, 0, 0)
WHITE = Pixel(255, 255, 255)
TRANSPARENT = Pixel(0, 0, 0, 0)


def bgra_data(*pixels: Pixel) -> bytes:
    """Converts the given pixels into a bgra byte array"""
    out = bytearray()
    for p in pixels:
        out.extend([p.blue, p.green, p.red, p.alpha])
    return bytes(out)


class ScreenshotImageTest(unittest.TestCase):
    """Unit tests for honeydew.affordances.ui.ScreenshotImage"""

    def test_ctor(self) -> None:
        image = ScreenshotImage(size=Size(1, 1), data=bgra_data(GREEN))
        self.assertEqual(image.size, Size(1, 1))
        self.assertEqual(image.data, bgra_data(GREEN))

    def test_ctor_empty_image(self) -> None:
        image = ScreenshotImage(size=Size(0, 0), data=b"")
        self.assertEqual(image.size, Size(0, 0))
        self.assertEqual(image.data, b"")

    def test_ctor_bad_args(self) -> None:
        with self.assertRaisesRegex(ValueError, "Invalid image size.*"):
            ScreenshotImage(size=Size(-1, 1), data=bgra_data(GREEN))

        with self.assertRaisesRegex(ValueError, "Invalid image size.*"):
            ScreenshotImage(size=Size(1, -1), data=bgra_data(GREEN))

        with self.assertRaisesRegex(
            ValueError, "Data length must be a multiple of 4"
        ):
            ScreenshotImage(size=Size(1, 1), data=bytes([0, 0, 0]))

        with self.assertRaisesRegex(ValueError, "Expected data length 24.*"):
            ScreenshotImage(
                size=Size(2, 3), data=bgra_data(GREEN, GREEN, GREEN, GREEN)
            )

    def test_save(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            image_file = pathlib.Path(tmpdir) / "image.bgra"
            image = ScreenshotImage(size=Size(1, 1), data=bgra_data(GREEN))
            image.save(str(image_file))
            self.assertEqual(image.data, image_file.read_bytes())

    def test_save_only_bgra_is_supported(self) -> None:
        image = ScreenshotImage(size=Size(1, 1), data=bgra_data(GREEN))
        with self.assertRaisesRegex(
            ValueError, "Only \\.bgra files are supported.*"
        ):
            image.save("foo.png")

    def test_load_from_path(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            image_file = pathlib.Path(tmpdir) / "image.bgra"

            # A 1x1 image with a green bgra pixel
            expected_bytes = bgra_data(GREEN)
            image_file.write_bytes(expected_bytes)
            image = ScreenshotImage.load_from_path(str(image_file))
            self.assertEqual(expected_bytes, image.data)

    def test_load_from_path_only_bgra_is_supported(self) -> None:
        with self.assertRaisesRegex(
            ValueError, "Only \\.bgra files are supported.*"
        ):
            ScreenshotImage.load_from_path("foo.png")

    def test_load_from_resource(self) -> None:
        # Unfortunately cannot just import the `resources` subpackage since
        # it has a different absolute package name when the test runs in `fx test`
        # (just "resources") and in `conformance.sh` (named `tests.unit_tests.affordances_tests.ui.resources`)
        # so instead we compute it relative to __name__.
        resources_package_name = ".".join(
            __name__.split(".")[0:-1] + ["resources"]
        )
        image = ScreenshotImage.load_from_resource(
            resources_package_name, "one_by_one_green.bgra"
        )
        self.assertEqual(image.size.width, 1)
        self.assertEqual(image.size.height, 1)
        self.assertEqual(image.data, bgra_data(GREEN))

    def test_load_from_resource_only_bgra_is_supported(self) -> None:
        with self.assertRaisesRegex(
            ValueError, "Only \\.bgra files are supported.*"
        ):
            ScreenshotImage.load_from_resource("foo", "bar.png")

    def test_get_pixel(self) -> None:
        image_bytes = bgra_data(RED, GREEN, BLUE, WHITE)
        image = ScreenshotImage(size=Size(2, 2), data=image_bytes)

        self.assertEqual(image.get_pixel(0, 0), RED)
        self.assertEqual(image.get_pixel(1, 0), GREEN)
        self.assertEqual(image.get_pixel(0, 1), BLUE)
        self.assertEqual(image.get_pixel(1, 1), WHITE)

        image = ScreenshotImage(size=Size(1, 1), data=bgra_data(RED))
        self.assertEqual(image.get_pixel(0, 0), RED)

    @parameterized.expand(
        [
            (2, 0),
            (0, 2),
            (-1, 0),
            (0, -1),
        ]
    )
    def test_get_pixel_out_of_bounds(self, x, y) -> None:
        image_bytes = bgra_data(RED, GREEN, BLUE, WHITE)
        image = ScreenshotImage(size=Size(2, 2), data=image_bytes)

        with self.assertRaisesRegex(
            ValueError, "Pixel coordinates.*are outside of image.*"
        ):
            image.get_pixel(x, y)

    def test_histogram(self):
        image_bytes = bgra_data(RED, RED, BLUE)
        image = ScreenshotImage(size=Size(3, 1), data=image_bytes)

        self.assertDictEqual(
            image.histogram(),
            {
                RED: 2,
                BLUE: 1,
            },
        )

    @parameterized.expand(
        [
            (
                [RED, WHITE, BLUE, GREEN],
                [RED, WHITE, BLUE, GREEN],
                1.0,
            ),
            (
                [RED, WHITE, BLUE, GREEN],
                [RED, WHITE, BLUE, BLACK],
                0.75,
            ),
            (
                [RED, WHITE, BLUE, GREEN],
                [WHITE, RED, GREEN, GREEN],
                0.25,
            ),
            (
                [RED, WHITE, BLUE, GREEN],
                [WHITE, RED, GREEN, RED],
                0.0,
            ),
        ]
    )
    def test_pixel_similarity(self, pixels1, pixels2, expected_similarity):
        image1 = ScreenshotImage(size=Size(2, 2), data=bgra_data(*pixels1))
        image2 = ScreenshotImage(size=Size(2, 2), data=bgra_data(*pixels2))
        self.assertEqual(
            image1.pixel_similarity(image2),
            expected_similarity,
        )

    def test_pixel_similarity_different_sized_images(self):
        image1 = ScreenshotImage(size=Size(1, 1), data=bgra_data(RED))
        image2 = ScreenshotImage(size=Size(2, 1), data=bgra_data(GREEN, RED))

        with self.assertRaises(ValueError):
            image1.pixel_similarity(image2)

    @parameterized.expand(
        [
            (
                [WHITE, RED, GREEN, BLUE],
                [RED, WHITE, BLUE, GREEN],
                1.0,
            ),
            (
                [WHITE, RED, TRANSPARENT, GREEN],
                [RED, WHITE, BLUE, BLACK],
                0.5,
            ),
            (
                [RED, RED, WHITE, WHITE],
                [BLACK, BLACK, BLACK, RED],
                0.25,
            ),
            (
                [RED, WHITE, BLUE, BLUE],
                [BLACK, GREEN, TRANSPARENT, TRANSPARENT],
                0.0,
            ),
        ]
    )
    def test_histogram_similarity(self, pixel1, pixels2, expected_similarity):
        image1 = ScreenshotImage(size=Size(2, 2), data=bgra_data(*pixel1))
        image2 = ScreenshotImage(size=Size(2, 2), data=bgra_data(*pixels2))
        self.assertEqual(
            image1.histogram_similarity(image2),
            expected_similarity,
        )

    def test_histogram_similarity_different_sized_images(self):
        image1 = ScreenshotImage(size=Size(1, 1), data=bgra_data(RED))
        image2 = ScreenshotImage(size=Size(2, 1), data=bgra_data(GREEN, RED))

        with self.assertRaises(ValueError):
            image1.histogram_similarity(image2)
