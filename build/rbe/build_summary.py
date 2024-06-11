#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Display a post-build summary of RBE metrics.
"""

import argparse
import collections
import os
import sys

import tablefmt

# Rather than depend on the proto (from reclient source),
import textpb
from pathlib import Path
from typing import Any, Dict, Iterable, Optional, Sequence, Tuple

_SCRIPT = Path(__file__)


def _main_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Display RBE metrics from a build.",
        argument_default=None,
    )

    # Positional args
    parser.add_argument(
        "reproxy_logdir",
        type=Path,
        help="The reproxy log dir of the build to summarize",
        metavar="DIR",
    )

    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def labels_to_dict(labels: str) -> Dict[str, str]:
    result = {}
    for label in labels.split(","):
        k, _, v = label.partition("=")
        result[k] = v
    return result


def get_action_category_from_labels(labels: str) -> str:
    labels_dict = labels_to_dict(labels)
    action_lang = labels_dict.get("lang", "")
    action_type = labels_dict.get("type", "")
    action_tool = labels_dict.get("tool", "")
    action_toolname = labels_dict.get("toolname", "")
    if action_lang == "cpp" and action_type == "compile":
        return "cxx"
    if action_tool == "clang" and action_type == "link":
        return "link"
    if action_toolname == "rustc":
        return "rust"

    return action_toolname or "other"


def get_action_category_and_metric(text: str) -> Tuple[Optional[str], str]:
    if not text.startswith("["):
        return None, text
    if "]." not in text:
        return None, text
    labels, _, metric = text.removeprefix("[").partition("].")
    if labels:
        return get_action_category_from_labels(labels), metric
    return None, metric


def _get_stat_name(stat: Dict[str, Any]) -> str:
    # based on textpb structure and stats.Stat_pb2 proto
    return stat["name"][0].text.strip('"')


def _get_stat_count(stat: Dict[str, Any]) -> int:
    return int(stat["count"][0].text)


def _counts_by_value_to_dict(fields: Iterable[Any]) -> Dict[str, int]:
    # based on the structure returned by textpb.parse()
    return {_get_stat_name(entry): _get_stat_count(entry) for entry in fields}


def build_metric_table(
    name: str,
    data_source: Dict[str, Dict[str, int]],  # [action_category][value]: count
    column_headers: Sequence[str],
    include_header: bool = True,
    with_totals: bool = False,
) -> Sequence[Sequence[Any]]:
    """Construct a numeric table of data (2D)."""
    rows = set()
    for v in data_source.values():
        rows.update(v.keys())

    ordered_rows = sorted(rows)

    totals_row = tablefmt.create_table(1, len(column_headers) + 1)
    totals_row[0] = "total"
    totals_row[1:] = [0 for _ in range(len(column_headers))]

    table = tablefmt.create_table(len(rows), len(column_headers) + 1)
    for r, row in enumerate(ordered_rows):
        table[r][0] = "  " + row  # visual hang-indentation
        for c, col in enumerate(column_headers):
            count = data_source[col].get(row, 0)
            table[r][c + 1] = count
            totals_row[c + 1] += count  # for the "total" row

    # Assemble the table with optional header/totals.
    final_table = []
    if include_header:
        final_table.append(tablefmt.make_table_header(column_headers, name))
    else:
        final_table.append(
            tablefmt.make_separator_row(len(column_headers), name)
        )

    final_table.extend(table)

    if with_totals:
        final_table.append(totals_row)

    return final_table


def build_metric_row(
    name: str,
    data_source: Dict[str, int],  # [action_category]: number
    column_headers: Sequence[str],
    include_header: bool = True,
) -> Sequence[Sequence[Any]]:
    """Construct one array of data."""
    row = tablefmt.create_row(len(column_headers) + 1)
    row[0] = name
    for c, col in enumerate(column_headers):
        row[c + 1] = data_source.get(col, 0)

    if include_header:
        return [tablefmt.make_table_header(column_headers), row]

    return [row]


def main(argv: Sequence[str]) -> int:
    args = _MAIN_ARG_PARSER.parse_args(argv)

    rbe_metrics_txt = args.reproxy_logdir / "rbe_metrics.txt"
    if not rbe_metrics_txt.exists():
        print("No RBE metrics found.")
        return 0

    with open(rbe_metrics_txt) as f:
        data = textpb.parse(f)

    if "stats" not in data:
        return 0

    stats = {_get_stat_name(stat): stat for stat in data["stats"]}

    # Construct a table by action type
    status_metrics = collections.defaultdict(dict)
    bandwidth_metrics = collections.defaultdict(dict)

    for name, fields in stats.items():
        # Extract labels from name (if applicable)
        action_category, metric_name = get_action_category_and_metric(name)
        action_category = action_category or "all"

        # Pick some interesting metrics to display
        if "Status" in metric_name:  # e.g. "Result.Status", "CompletionStatus"
            counts_by_value = _counts_by_value_to_dict(
                fields["counts_by_value"]
            )
            status_metrics[metric_name][action_category] = counts_by_value

        if "Downloaded" in metric_name or "Uploaded" in metric_name:
            # pre-format size numbers into readable str.
            # No need for any math operations on these values.
            readable_size = tablefmt.human_readable_size(
                _get_stat_count(fields), "B", 1
            )
            bandwidth_metrics[metric_name][action_category] = readable_size

    # Prepare table columns, by action categories.
    action_categories = {k for v in status_metrics.values() for k in v.keys()}
    action_categories.remove("all")  # "all" is special, always in last position
    ordered_action_categories = sorted(action_categories) + ["all"]

    shared_header = tablefmt.make_table_header(
        ordered_action_categories, "[by action type]"
    )
    blank_row = tablefmt.make_separator_row(len(ordered_action_categories))

    # Align cells across multiple tables by viewing as a single table.
    # All cell values in these tables are numeric, and thus, right-aligned.
    joint_table = (
        [shared_header]
        + build_metric_table(
            "CompletionStatus",
            status_metrics["CompletionStatus"],
            ordered_action_categories,
            include_header=False,
            with_totals=True,
        )
        + [blank_row]
        + build_metric_row(
            "BytesDownloaded",
            bandwidth_metrics["RemoteMetadata.RealBytesDownloaded"],
            ordered_action_categories,
            include_header=False,
        )
        + build_metric_row(
            "BytesUploaded",
            bandwidth_metrics["RemoteMetadata.RealBytesUploaded"],
            ordered_action_categories,
            include_header=False,
        )
    )

    # Render multi-table.
    script_rel = os.path.relpath(str(_SCRIPT), start=os.curdir)
    print(
        f"=== Remote build summary (from: {script_rel} {args.reproxy_logdir})"
    )
    for row in tablefmt.format_numeric_table(joint_table):
        print(row)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
