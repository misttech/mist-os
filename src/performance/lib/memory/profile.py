# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import fnmatch
import json
from typing import cast

from honeydew.fuchsia_device.fuchsia_device import FuchsiaDevice
from trace_processing import trace_metrics
from trace_processing.trace_metrics import JSON


class MemoryProfileMetrics(trace_metrics.ConstantMetricsProcessor):
    """Captures kernel and user space memory metrics using `ffx profile memory`.
    See documentation at
    https://fuchsia.dev/fuchsia-src/development/tools/ffx/workflows/explore-memory-usage

    process_metrics() returns a list of metrics to be exported to catapult:
    - "Memory/Process/{starnix_kernel,binder}.PrivatePopulated": total populated
    bytes for VMOs. This is private uncompressed memory.

    process_freeform_metrics() returns a whole-device memory digest, as
    retrieved by `ffx profile memory`.
    """

    DESCRIPTION_BASE = (
        "Total populated bytes for private uncompressed memory VMOs"
    )


def capture_and_compute_metrics(
    dut: FuchsiaDevice,
) -> trace_metrics.ConstantMetricsProcessor:
    """Captures kernel and user space memory metrics using `ffx profile memory`.

    Returns a MemoryProfileMetrics instance containing two sets of memory
    measurements:
    - Total populated bytes for VMOs assigned to the binder and the starnix
      kernel. This is private uncompressed memory.
    - A whole-device memory digest.
    """
    profile = json.loads(
        dut.ffx.run(
            ["--machine", "json", "profile", "memory", "--buckets"],
        )
    )

    metrics = []
    for process_group_name, process_name_pattern in {
        "starnix_kernel": "starnix_kernel.cm",
        "binder": "binder:*",
    }.items():
        private_populated = sum(
            proc["memory"]["private_populated"]
            for proc in profile["CompleteDigest"]["processes"]
            if fnmatch.fnmatch(proc["name"], process_name_pattern)
        )
        metrics.append(
            trace_metrics.TestCaseResult(
                label=f"Memory/Process/{process_group_name}/PrivatePopulated",
                unit=trace_metrics.Unit.bytes,
                values=[private_populated],
                doc=(
                    f"{MemoryProfileMetrics.DESCRIPTION_BASE}: "
                    f"{process_group_name}"
                ),
            )
        )

    freeform_metrics = _simplify_digest(profile["CompleteDigest"])
    return MemoryProfileMetrics(
        metrics=metrics, freeform_metrics=("memory_profile", freeform_metrics)
    )


def _simplify_bucket(bucket: JSON) -> dict[str, JSON]:
    """Prepares `ffx profile memory` JSON data for BigQuery.

    Input sample:
        {
            "name": "ZBI Buffer",
            "size": 0,
            "vmos": []
        }

    Output sample:
        {
            "name": "ZBI Buffer",
            "size_in_bytes": 0
        }
    """
    if not isinstance(bucket, dict):
        raise ValueError(f"Expects a dict but found {bucket!r}")
    return dict(name=bucket["name"], size_in_bytes=bucket["size"])


def _simplify_buckets(buckets: JSON) -> list[JSON]:
    """Prepares `ffx profile memory` JSON data for BigQuery.

    Turns a list of buckets into a list of simplified buckets.
    """
    if not isinstance(buckets, list):
        raise ValueError(f"Expects a list but found {buckets!r}")
    return [_simplify_bucket(b) for b in buckets]


def _simplify_name_to_vmo_memory(name_to_vmo_memory: JSON) -> list[JSON]:
    """Prepares `ffx profile memory` JSON data for BigQuery.

    Input sample:
        {
            "[blobs]": {
                "private": 0,
                "private_populated": 0,
                "scaled": 296959744,
                "scaled_populated": 296959744,
                "total": 871759872,
                "total_populated": 871759872,
                "vmos": [
                    153096,
                    127992
                ]
            }
        }

    Output sample:
        {
            "name": "[blobs]",
            "private": 0,
            "private_populated": 0,
            "scaled": 296959744,
            "scaled_populated": 296959744,
            "total": 871759872,
            "total_populated": 871759872,
        }
    """
    if not isinstance(name_to_vmo_memory, dict):
        raise ValueError
    return [
        dict(name=cast(JSON, k)) | _with_vmos_removed(v)
        for k, v in name_to_vmo_memory.items()
    ]


def _simplify_process(process: JSON) -> dict[str, JSON]:
    """Prepares `ffx profile memory` JSON data for BigQuery.

    Input sample:
        {
            "koid": 37989,
            "name": "starnix_kernel.cm",
            "memory": {
                "private": 124809216,
                "private_populated": 301281280,
                "scaled": 463242577,
                "scaled_populated": 640658601,
                "total": 1138892800,
                "total_populated": 1318207488,
                "vmos": []
            },
            "name_to_vmo_memory": { ... }
        }

    Output sample:
        {
            "name": "starnix_kernel.cm",
            "memory": {
                "private": 124809216,
                "private_populated": 301281280,
                "scaled": 463242577,
                "scaled_populated": 640658601,
                "total": 1138892800,
                "total_populated": 1318207488,
            },
            "name_to_vmo_memory": { ... }
        }
    """
    if not isinstance(process, dict):
        raise ValueError
    return dict(
        name=process["name"],
        memory=_with_vmos_removed(process["memory"]),
        vmo_groups=_simplify_name_to_vmo_memory(process["name_to_vmo_memory"]),
    )


def _simplify_processes(processes: JSON) -> list[JSON]:
    """Prepares `ffx profile memory` JSON data for BigQuery.

    Turns a list of processes and a list of simplified processes.
    """
    if not isinstance(processes, list):
        raise ValueError
    return [_simplify_process(b) for b in processes]


def _with_vmos_removed(metrics_dict: JSON) -> dict[str, JSON]:
    """Returns a copy of the specified directory without the "vmos" key."""
    if not isinstance(metrics_dict, dict):
        raise ValueError
    return {k: v for k, v in metrics_dict.items() if k != "vmos"}


def _simplify_digest(digest: JSON) -> JSON:
    if not isinstance(digest, dict):
        raise ValueError
    return dict(
        buckets=_simplify_buckets(digest["buckets"]),
        processes=_simplify_processes(digest["processes"]),
    )
