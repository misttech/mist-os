# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import pathlib
import tempfile
import unittest

import generate_idk

UNMERGABLE_TYPES = [
    "cc_source_library",
    "dart_library",
    "fidl_library",
    "documentation",
    "config",
    "license",
    "component_manifest",
    "bind_library",
    "version_history",
    "experimental_python_e2e_test",
]


class GenerateIdkTests(unittest.TestCase):
    def test_unmergables_error(self) -> None:
        for atom_type in UNMERGABLE_TYPES:
            a: generate_idk.PartialIDK = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "foo/bar.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/foo/bar.json"),
                        meta={
                            "type": atom_type,
                            "foo": "bar",
                        },
                        dest_to_src={},
                    )
                },
            )
            b: generate_idk.PartialIDK = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "foo/bar.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/foo/bar.json"),
                        meta={
                            "type": atom_type,
                            "foo": "baz",
                        },
                        dest_to_src={},
                    )
                },
            )

            with self.assertRaises(generate_idk.AtomMergeError) as e:
                generate_idk.MergedIDK().merge_with(a).merge_with(b)
            self.assertIn("'foo' does not match", str(e.exception.__cause__))

    def test_unmergables_pass(self) -> None:
        for atom_type in UNMERGABLE_TYPES:
            a: generate_idk.PartialIDK = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "foo/bar.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/foo/bar.json"),
                        meta={
                            "type": atom_type,
                            "foo": "bar",
                        },
                        dest_to_src={},
                    ),
                    "foo/qux.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/foo/qux.json"),
                        meta={
                            "type": "cc_source_library",
                            "hither": "yon",
                        },
                        dest_to_src={},
                    ),
                },
            )
            b: generate_idk.PartialIDK = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "foo/bar.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/foo/bar.json"),
                        meta={
                            "type": atom_type,
                            "foo": "bar",
                        },
                        dest_to_src={},
                    ),
                    "foo/baz.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/foo/baz.json"),
                        meta={
                            "type": atom_type,
                            "foo": "baz",
                        },
                        dest_to_src={},
                    ),
                },
            )

            self.assertEqual(
                generate_idk.MergedIDK().merge_with(a).merge_with(b),
                generate_idk.MergedIDK(
                    atoms={
                        "foo/bar.json": {
                            "type": atom_type,
                            "foo": "bar",
                        },
                        "foo/baz.json": {
                            "type": atom_type,
                            "foo": "baz",
                        },
                        "foo/qux.json": {
                            "type": "cc_source_library",
                            "hither": "yon",
                        },
                    },
                    dest_to_src={},
                ),
            )

    def test_merge_cc_prebuilt_library_pass(self) -> None:
        a = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "cc_prebuilt_library",
                        "binaries": {
                            "a": "a",
                        },
                        "variants": [
                            {"constraints": "a"},
                        ],
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )
        b = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "cc_prebuilt_library",
                        "binaries": {
                            "b": "b",
                            "c": "c",
                        },
                        "variants": [
                            {"constraints": "b"},
                            {"constraints": "c"},
                        ],
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )

        self.assertEqual(
            generate_idk.MergedIDK().merge_with(a).merge_with(b),
            generate_idk.MergedIDK(
                atoms={
                    "atom0.json": {
                        "type": "cc_prebuilt_library",
                        "binaries": {
                            "a": "a",
                            "b": "b",
                            "c": "c",
                        },
                        "variants": [
                            {"constraints": "a"},
                            {"constraints": "b"},
                            {"constraints": "c"},
                        ],
                        "foo": "bar",
                    },
                },
                dest_to_src={},
            ),
        )

    def test_merge_package_pass(self) -> None:
        a = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "package",
                        "variants": [
                            {"api_level": 12, "arch": "x64"},
                            {"api_level": 13, "arch": "x64"},
                        ],
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )
        b = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "package",
                        "variants": [
                            {"api_level": 13, "arch": "arm64"},
                            {"api_level": 14, "arch": "arm64"},
                        ],
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )

        self.assertEqual(
            generate_idk.MergedIDK().merge_with(a).merge_with(b),
            generate_idk.MergedIDK(
                atoms={
                    "atom0.json": {
                        "type": "package",
                        "variants": [
                            {"api_level": 12, "arch": "x64"},
                            {"api_level": 13, "arch": "x64"},
                            {"api_level": 13, "arch": "arm64"},
                            {"api_level": 14, "arch": "arm64"},
                        ],
                        "foo": "bar",
                    },
                },
                dest_to_src={},
            ),
        )

    def test_merge_loadable_module_pass(self) -> None:
        a = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "loadable_module",
                        "binaries": {
                            "a": "a",
                        },
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )
        b = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "loadable_module",
                        "binaries": {
                            "b": "b",
                            "c": "c",
                        },
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )

        self.assertEqual(
            generate_idk.MergedIDK().merge_with(a).merge_with(b),
            generate_idk.MergedIDK(
                atoms={
                    "atom0.json": {
                        "type": "loadable_module",
                        "binaries": {
                            "a": "a",
                            "b": "b",
                            "c": "c",
                        },
                        "foo": "bar",
                    },
                },
                dest_to_src={},
            ),
        )

    def test_merge_sysroot_pass(self) -> None:
        a = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "sysroot",
                        "versions": {
                            "a": "a",
                        },
                        "variants": [
                            {"constraints": "a"},
                        ],
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )
        b = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "sysroot",
                        "versions": {
                            "b": "b",
                            "c": "c",
                        },
                        "variants": [
                            {"constraints": "b"},
                            {"constraints": "c"},
                        ],
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )

        self.assertEqual(
            generate_idk.MergedIDK().merge_with(a).merge_with(b),
            generate_idk.MergedIDK(
                atoms={
                    "atom0.json": {
                        "type": "sysroot",
                        "versions": {
                            "a": "a",
                            "b": "b",
                            "c": "c",
                        },
                        "variants": [
                            {"constraints": "a"},
                            {"constraints": "b"},
                            {"constraints": "c"},
                        ],
                        "foo": "bar",
                    },
                },
                dest_to_src={},
            ),
        )

    def test_merge_variants_cc_prebuilt_library_pass(self) -> None:
        a = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "cc_prebuilt_library",
                        "variants": [
                            {"constraints": "a"},
                        ],
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )
        b = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "cc_prebuilt_library",
                        "variants": [
                            {"constraints": "b"},
                            {"constraints": "c"},
                        ],
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )

        self.assertEqual(
            generate_idk.MergedIDK().merge_with(a).merge_with(b),
            generate_idk.MergedIDK(
                atoms={
                    "atom0.json": {
                        "type": "cc_prebuilt_library",
                        "binaries": {},
                        "variants": [
                            {"constraints": "a"},
                            {"constraints": "b"},
                            {"constraints": "c"},
                        ],
                        "foo": "bar",
                    },
                },
                dest_to_src={},
            ),
        )

    def test_merge_binaries_fail(self) -> None:
        for atom_type in ["cc_prebuilt_library", "loadable_module"]:
            a = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "atom0.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/atom0.json"),
                        meta={
                            "type": atom_type,
                            "binaries": {
                                "a": "a",
                            },
                            "foo": "bar",
                        },
                        dest_to_src={},
                    ),
                },
            )
            b = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "atom0.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/atom0.json"),
                        meta={
                            "type": atom_type,
                            "binaries": {
                                "a": "a",
                                "c": "c",
                            },
                            "foo": "bar",
                        },
                        dest_to_src={},
                    ),
                },
            )

            with self.assertRaises(generate_idk.AtomMergeError) as e:
                generate_idk.MergedIDK().merge_with(a).merge_with(b)
            self.assertIn("overlapping keys", str(e.exception.__cause__))

    def test_merge_versions_fail(self) -> None:
        a = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "sysroot",
                        "versions": {
                            "a": "a",
                        },
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )
        b = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "sysroot",
                        "versions": {
                            "a": "a",
                            "c": "c",
                        },
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )

        with self.assertRaises(generate_idk.AtomMergeError) as e:
            generate_idk.MergedIDK().merge_with(a).merge_with(b)
        self.assertIn("overlapping keys", str(e.exception.__cause__))

    def test_merge_variants_fail(self) -> None:
        for atom_type in ["cc_prebuilt_library", "sysroot"]:
            a = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "atom0.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/atom0.json"),
                        meta={
                            "type": atom_type,
                            "variants": [
                                {"constraints": "a"},
                            ],
                            "foo": "bar",
                        },
                        dest_to_src={},
                    ),
                },
            )
            b = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "atom0.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/atom0.json"),
                        meta={
                            "type": atom_type,
                            "variants": [
                                {"constraints": "a"},
                                {"constraints": "c"},
                            ],
                            "foo": "bar",
                        },
                        dest_to_src={},
                    ),
                },
            )

            with self.assertRaises(generate_idk.AtomMergeError) as e:
                generate_idk.MergedIDK().merge_with(a).merge_with(b)
            self.assertIn("duplicate variants", str(e.exception.__cause__))

    def test_merge_variants_package_fail(self) -> None:
        a = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "package",
                        "variants": [
                            {"api_level": 12, "arch": "x64"},
                        ],
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )
        b = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "atom0.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/atom0.json"),
                    meta={
                        "type": "package",
                        "variants": [
                            {"api_level": 12, "arch": "x64"},
                            {"api_level": 12, "arch": "arm64"},
                        ],
                        "foo": "bar",
                    },
                    dest_to_src={},
                ),
            },
        )

        with self.assertRaises(generate_idk.AtomMergeError) as e:
            generate_idk.MergedIDK().merge_with(a).merge_with(b)
        self.assertIn("duplicate variants", str(e.exception.__cause__))

    def test_merge_files(self) -> None:
        a: generate_idk.PartialIDK = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "foo/bar.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/foo/bar.json"),
                    meta={
                        "type": "cc_source_library",
                    },
                    dest_to_src={
                        pathlib.Path("dest/foo"): pathlib.Path("src/foo"),
                        pathlib.Path("dest/bar"): pathlib.Path("src/bar"),
                    },
                )
            },
        )
        b: generate_idk.PartialIDK = generate_idk.PartialIDK(
            manifest_src="thingy.sdk",
            atoms={
                "foo/bar.json": generate_idk.PartialAtom(
                    meta_src=pathlib.Path("src/foo/bar.json"),
                    meta={
                        "type": "cc_source_library",
                    },
                    dest_to_src={
                        pathlib.Path("dest/foo"): pathlib.Path("src/foo"),
                        pathlib.Path("dest/baz"): pathlib.Path("src/baz"),
                    },
                )
            },
        )
        print(generate_idk.MergedIDK().merge_with(a))
        print(generate_idk.MergedIDK().merge_with(a).merge_with(b))

        self.assertEqual(
            generate_idk.MergedIDK().merge_with(a).merge_with(b),
            generate_idk.MergedIDK(
                atoms={
                    "foo/bar.json": {
                        "type": "cc_source_library",
                    },
                },
                dest_to_src={
                    pathlib.Path("dest/foo"): pathlib.Path("src/foo"),
                    pathlib.Path("dest/bar"): pathlib.Path("src/bar"),
                    pathlib.Path("dest/baz"): pathlib.Path("src/baz"),
                },
            ),
        )

    def test_merge_files_check_equality_pass(self) -> None:
        with tempfile.TemporaryDirectory() as dir:
            d = pathlib.Path(dir)

            file_a = d / "a.txt"
            file_b = d / "b.txt"

            file_a.write_text("foo")
            file_b.write_text("foo")

            a = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "foo/bar.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/foo/bar.json"),
                        meta={
                            "type": "cc_source_library",
                        },
                        dest_to_src={
                            pathlib.Path("dest/foo"): file_a,
                        },
                    ),
                },
            )
            b = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "foo/bar.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/foo/bar.json"),
                        meta={
                            "type": "cc_source_library",
                        },
                        dest_to_src={
                            pathlib.Path("dest/foo"): file_b,
                        },
                    ),
                },
            )
            self.assertEqual(
                generate_idk.MergedIDK().merge_with(a).merge_with(b),
                generate_idk.MergedIDK(
                    atoms={
                        "foo/bar.json": {
                            "type": "cc_source_library",
                        },
                    },
                    dest_to_src={
                        pathlib.Path("dest/foo"): file_a,
                    },
                ),
            )

    def test_merge_files_check_equality_fail(self) -> None:
        with tempfile.TemporaryDirectory() as dir:
            d = pathlib.Path(dir)

            file_a = d / "a.txt"
            file_b = d / "b.txt"

            file_a.write_text("foo")
            file_b.write_text("bar")

            a = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "foo/bar.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/foo/bar.json"),
                        meta={
                            "type": "cc_source_library",
                        },
                        dest_to_src={
                            pathlib.Path("dest/foo"): file_a,
                        },
                    ),
                },
            )
            b = generate_idk.PartialIDK(
                manifest_src="thingy.sdk",
                atoms={
                    "foo/bar.json": generate_idk.PartialAtom(
                        meta_src=pathlib.Path("src/foo/bar.json"),
                        meta={
                            "type": "cc_source_library",
                        },
                        dest_to_src={
                            pathlib.Path("dest/foo"): file_b,
                        },
                    ),
                },
            )

            with self.assertRaises(AssertionError) as e:
                generate_idk.MergedIDK().merge_with(a).merge_with(b)
            self.assertIn("Multiple non-identical files", str(e.exception))


if __name__ == "__main__":
    unittest.main()
