# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Test for profile.py"""

import unittest
from unittest.mock import Mock

from memory import profile
from trace_processing import trace_metrics


class ProfileTest(unittest.TestCase):
    def test_capture_and_compute_metrics(self) -> None:
        dut = Mock()
        dut.ffx.run.return_value = """
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
                },
            ),
        )
