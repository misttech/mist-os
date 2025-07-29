# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Test for profile.py"""

import unittest
from unittest.mock import Mock

from honeydew.transports.ffx import errors as ffx_errors
from memory import profile
from trace_processing import trace_metrics

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
            if backend == "memory_monitor_1":
                raise ffx_errors.FfxCommandError("Boom")
            return MM2_OUTPUT

        dut = Mock()
        dut.ffx.run.side_effect = ffx_run_fake_implementation
        metrics_processor = profile.capture(
            dut, principal_groups={"fxfs": "*/fxfs"}
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
                            "vmos": [
                                {
                                    "name": "[blobs]",
                                    "count": 1827,
                                    "committed_private": 0,
                                    "committed_scaled": 411303842.98151606,
                                    "committed_total": 1385910272,
                                    "populated_private": 0,
                                    "populated_scaled": 411303842.98151606,
                                    "populated_total": 1385910272,
                                }
                            ],
                        }
                    ]
                },
            ),
        )
