# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for aggregating size reports."""

load(":providers.bzl", "FuchsiaSizeCheckerInfo")
load(":utils.bzl", "LOCAL_ONLY_ACTION_KWARGS")

def _fuchsia_size_report_aggregator_impl(ctx):
    size_budgets = ",".join([
        report[FuchsiaSizeCheckerInfo].size_budgets.path
        for report in ctx.attr.size_reports
        if hasattr(report[FuchsiaSizeCheckerInfo], "size_budgets")
    ])
    size_reports = ",".join([
        report[FuchsiaSizeCheckerInfo].size_report.path
        for report in ctx.attr.size_reports
        if hasattr(report[FuchsiaSizeCheckerInfo], "size_report")
    ])
    verbose_outputs = ",".join([
        report[FuchsiaSizeCheckerInfo].verbose_output.path
        for report in ctx.attr.size_reports
        if hasattr(report[FuchsiaSizeCheckerInfo], "verbose_output")
    ])

    size_budgets_file = ctx.actions.declare_file(ctx.label.name + "_size_budgets.json")
    size_report_file = ctx.actions.declare_file(ctx.label.name + "_size_report.json")
    verbose_output_file = ctx.actions.declare_file(ctx.label.name + "_verbose_output.json")

    # Merge size reports and verbose outputs
    _merge_arguments = [
        "--merged-size-budgets",
        size_budgets_file.path,
        "--merged-size-reports",
        size_report_file.path,
        "--merged-verbose-outputs",
        verbose_output_file.path,
    ]
    if size_budgets:
        _merge_arguments += [
            "--size-budgets",
            size_budgets,
        ]
    if size_reports:
        _merge_arguments += [
            "--size-reports",
            size_reports,
        ]
    if verbose_outputs:
        _merge_arguments += [
            "--verbose-outputs",
            verbose_outputs,
        ]

    ctx.actions.run(
        outputs = [size_budgets_file, size_report_file, verbose_output_file],
        inputs = ctx.files.size_reports,
        executable = ctx.executable._size_report_merger,
        arguments = _merge_arguments,
        **LOCAL_ONLY_ACTION_KWARGS
    )

    deps = [size_budgets_file, size_report_file, verbose_output_file]

    fuchsia_size_checker_info = FuchsiaSizeCheckerInfo(
        size_budgets = size_budgets_file,
        size_report = size_report_file,
        verbose_output = verbose_output_file,
    )

    return [
        DefaultInfo(files = depset(direct = deps)),
        fuchsia_size_checker_info,
    ]

fuchsia_size_report_aggregator = rule(
    doc = """Declares a target for creating an aggregated size report.""",
    implementation = _fuchsia_size_report_aggregator_impl,
    provides = [FuchsiaSizeCheckerInfo],
    attrs = {
        "size_reports": attr.label_list(
            doc = "size reports that needs to be aggregated.",
            providers = [FuchsiaSizeCheckerInfo],
            mandatory = True,
        ),
        "_size_report_merger": attr.label(
            default = "//fuchsia/tools:size_report_merger",
            executable = True,
            cfg = "exec",
        ),
    },
)
