# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Test for profile.py"""

import unittest
from unittest.mock import Mock

from honeydew.transports.ffx import errors as ffx_errors
from memory import profile
from trace_processing import trace_metrics

MM1_OUTPUT = """
    {
        "CompleteDigest": {
            "time": 507738623455,
            "total_committed_bytes_in_vmos": 2211717120,
            "kernel": {
                "total": 8588746752,
                "free": 5601583104,
                "wired": 104804352,
                "total_heap": 194052096,
                "free_heap": 17161112,
                "vmo": 32919552,
                "vmo_pager_total": 1956556800,
                "vmo_pager_newest": 67706880,
                "vmo_pager_oldest": 1807450112,
                "vmo_discardable_locked": 0,
                "vmo_discardable_unlocked": 0,
                "mmu": 17190912,
                "ipc": 180224,
                "other": 423813120,
                "zram_compressed_total": null,
                "zram_uncompressed": null,
                "zram_fragmentation": null
            },
            "processes": [
                {
                    "koid": 37989,
                    "name": "starnix_kernel.cm",
                    "memory": {
                        "private": 86425600,
                        "private_populated": 303595520,
                        "scaled": 417590872,
                        "scaled_populated": 635797784,
                        "total": 1089761280,
                        "total_populated": 1310212096,
                        "vmos": []
                    },
                    "name_to_vmo_memory": {
                        "[data]": {
                            "private": 49152,
                            "private_populated": 90112,
                            "scaled": 49152,
                            "scaled_populated": 90112,
                            "total": 49152,
                            "total_populated": 90112,
                            "vmos": [
                                38082
                            ]
                        },
                        "[scudo]": {
                            "private": 11857920,
                            "private_populated": 81506304,
                            "scaled": 11857920,
                            "scaled_populated": 81506304,
                            "total": 11857920,
                            "total_populated": 81506304,
                            "vmos": [
                                162942
                            ]
                        }
                    },
                    "vmos": [
                        161706
                    ]
                }
            ],
            "vmos": [
                {
                    "koid": 237045,
                    "name": ".bss",
                    "parent_koid": 136843,
                    "committed_bytes": 0,
                    "allocated_bytes": 4096,
                    "populated_bytes": 0
                }
            ],
            "buckets": [
                {
                    "name": "ZBI Buffer",
                    "size": 0,
                    "vmos": []
                }
            ],
            "total_undigested": 416051200
        }
    }"""
MM2_OUTPUT = """
    {
        "ComponentDigest": {
            "kernel": {
            "memory_statistics": {
                "total_bytes": 8588746752,
                "free_bytes": 5056327680,
                "wired_bytes": 105005056,
                "total_heap_bytes": 179408896,
                "free_heap_bytes": 30892424,
                "vmo_bytes": 3228966912,
                "mmu_overhead_bytes": 15671296,
                "ipc_bytes": 204800,
                "other_bytes": 0,
                "free_loaned_bytes": 0,
                "cache_bytes": 1363968,
                "slab_bytes": 1798144,
                "zram_bytes": 0,
                "vmo_reclaim_total_bytes": 1575804928,
                "vmo_reclaim_newest_bytes": 19083264,
                "vmo_reclaim_oldest_bytes": 1555120128,
                "vmo_reclaim_disabled_bytes": 0,
                "vmo_discardable_locked_bytes": 0,
                "vmo_discardable_unlocked_bytes": 0
            },
            "compression_statistics": {
                "uncompressed_storage_bytes": 0,
                "compressed_storage_bytes": 0,
                "compressed_fragmentation_bytes": 0,
                "compression_time": 0,
                "decompression_time": 0,
                "total_page_compression_attempts": 0,
                "failed_page_compression_attempts": 0,
                "total_page_decompressions": 0,
                "compressed_page_evictions": 0,
                "eager_page_compressions": 0,
                "memory_pressure_page_compressions": 0,
                "critical_memory_page_compressions": 0,
                "pages_decompressed_unit_ns": 0,
                "pages_decompressed_within_log_time": [
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0
                ]
            }
            },
            "principals": [
            {
                "id": 5,
                "name": "bootstrap/fshost/fxfs",
                "principal_type": "R",
                "committed_private": 32927744,
                "committed_scaled": 450043948.85779566,
                "committed_total": 1436753920,
                "populated_private": 32927744,
                "populated_scaled": 450043948.85779566,
                "populated_total": 1436753920,
                "attributor": "root",
                "processes": [
                "fxfs.cm (13934)"
                ],
                "vmos": {
                "[blobs]": {
                    "count": 1827,
                    "committed_private": 0,
                    "committed_scaled": 411303842.98151606,
                    "committed_total": 1385910272,
                    "populated_private": 0,
                    "populated_scaled": 411303842.98151606,
                    "populated_total": 1385910272
                }
                }
            }
            ],
            "undigested": 0
        }
    }
"""


class ProfileTest(unittest.TestCase):
    def test_capture_and_compute_metrics(self) -> None:
        def ffx_run_fake_implementation(args: list[str]) -> str:
            backend = args[args.index("--backend") + 1]
            return {
                "memory_monitor_1": MM1_OUTPUT,
                "memory_monitor_2": MM2_OUTPUT,
            }[backend]

        dut = Mock()
        dut.ffx.run.side_effect = ffx_run_fake_implementation
        metrics_processor = profile.capture_and_compute_metrics(
            {
                "starnix_kernel": "starnix_kernel.cm",
                "binder": "binder:*",
            },
            dut,
        )

        model = Mock()
        self.assertEqual(
            metrics_processor.process_metrics(model),
            [
                trace_metrics.TestCaseResult(
                    "Memory/Process/starnix_kernel/PrivatePopulated",
                    unit=trace_metrics.Unit.bytes,
                    values=[303595520],
                    doc=(
                        f"{profile.MemoryProfileMetrics.DESCRIPTION_BASE}: "
                        "starnix_kernel"
                    ),
                ),
                trace_metrics.TestCaseResult(
                    "Memory/Process/binder/PrivatePopulated",
                    unit=trace_metrics.Unit.bytes,
                    values=[0],
                    doc=(
                        f"{profile.MemoryProfileMetrics.DESCRIPTION_BASE}: "
                        "binder"
                    ),
                ),
            ],
        )
        print(metrics_processor.process_freeform_metrics(model))
        self.assertEqual(
            metrics_processor.process_freeform_metrics(model),
            (
                "memory_profile",
                {
                    "buckets": [{"name": "ZBI Buffer", "size_in_bytes": 0}],
                    "processes": [
                        {
                            "name": "starnix_kernel.cm",
                            "memory": {
                                "private": 86425600,
                                "private_populated": 303595520,
                                "scaled": 417590872,
                                "scaled_populated": 635797784,
                                "total": 1089761280,
                                "total_populated": 1310212096,
                            },
                            "vmo_groups": [
                                {
                                    "name": "[data]",
                                    "private": 49152,
                                    "private_populated": 90112,
                                    "scaled": 49152,
                                    "scaled_populated": 90112,
                                    "total": 49152,
                                    "total_populated": 90112,
                                },
                                {
                                    "name": "[scudo]",
                                    "private": 11857920,
                                    "private_populated": 81506304,
                                    "scaled": 11857920,
                                    "scaled_populated": 81506304,
                                    "total": 11857920,
                                    "total_populated": 81506304,
                                },
                            ],
                        }
                    ],
                    "principals": [
                        {
                            "id": 5,
                            "name": "bootstrap/fshost/fxfs",
                            "principal_type": "R",
                            "committed_private": 32927744,
                            "committed_scaled": 450043948.85779566,
                            "committed_total": 1436753920,
                            "populated_private": 32927744,
                            "populated_scaled": 450043948.85779566,
                            "populated_total": 1436753920,
                            "attributor": "root",
                            "processes": ["fxfs.cm (13934)"],
                            "vmos": {
                                "[blobs]": {
                                    "count": 1827,
                                    "committed_private": 0,
                                    "committed_scaled": 411303842.98151606,
                                    "committed_total": 1385910272,
                                    "populated_private": 0,
                                    "populated_scaled": 411303842.98151606,
                                    "populated_total": 1385910272,
                                }
                            },
                        }
                    ],
                },
            ),
        )

    def test_capture_and_compute_metrics_without_mm1(self) -> None:
        def ffx_run_fake_implementation(args: list[str]) -> str:
            backend = args[args.index("--backend") + 1]
            if backend == "memory_monitor_1":
                raise ffx_errors.FfxCommandError("Boom")
            return MM2_OUTPUT

        dut = Mock()
        dut.ffx.run.side_effect = ffx_run_fake_implementation
        metrics_processor = profile.capture_and_compute_metrics(
            {}, dut, {"fxfs": "*/fxfs"}
        )

        model = Mock()
        self.assertEqual(
            metrics_processor.process_metrics(model),
            [
                trace_metrics.TestCaseResult(
                    label="Memory/Principal/fxfs/PrivatePopulated",
                    unit=trace_metrics.Unit.bytes,
                    values=(32927744,),
                    doc="Total populated bytes for private uncompressed memory "
                    "VMOs: fxfs",
                )
            ],
        )
        print(metrics_processor.process_freeform_metrics(model))
        self.assertEqual(
            metrics_processor.process_freeform_metrics(model),
            (
                "memory_profile",
                {
                    "principals": [
                        {
                            "id": 5,
                            "name": "bootstrap/fshost/fxfs",
                            "principal_type": "R",
                            "committed_private": 32927744,
                            "committed_scaled": 450043948.85779566,
                            "committed_total": 1436753920,
                            "populated_private": 32927744,
                            "populated_scaled": 450043948.85779566,
                            "populated_total": 1436753920,
                            "attributor": "root",
                            "processes": ["fxfs.cm (13934)"],
                            "vmos": {
                                "[blobs]": {
                                    "count": 1827,
                                    "committed_private": 0,
                                    "committed_scaled": 411303842.98151606,
                                    "committed_total": 1385910272,
                                    "populated_private": 0,
                                    "populated_scaled": 411303842.98151606,
                                    "populated_total": 1385910272,
                                }
                            },
                        }
                    ]
                },
            ),
        )
