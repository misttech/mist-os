# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for aggregating size reports."""

load(":providers.bzl", "FuchsiaSizeCheckerInfo")

def _fuchsia_size_report_aggregator_impl(ctx):
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

    size_report_file = ctx.actions.declare_file(ctx.label.name + "_size_report.json")
    verbose_output_file = ctx.actions.declare_file(ctx.label.name + "_verbose_output.json")

    # Merge size reports and verbose outputs
    ctx.actions.run(
        outputs = [size_report_file, verbose_output_file],
        inputs = ctx.files.size_reports,
        executable = ctx.executable._size_report_merger,
        arguments = [
            "--size-reports",
            size_reports,
            "--verbose-outputs",
            verbose_outputs,
            "--merged-size-reports",
            size_report_file.path,
            "--merged-verbose-outputs",
            verbose_output_file.path,
        ],
    )

    deps = [size_report_file, verbose_output_file]

    return [
        DefaultInfo(files = depset(direct = deps)),
        FuchsiaSizeCheckerInfo(
            size_report = size_report_file,
            verbose_output = verbose_output_file,
        ),
    ]

fuchsia_size_report_aggregator = rule(
    doc = """Create an aggregated size report.""",
    implementation = _fuchsia_size_report_aggregator_impl,
    provides = [FuchsiaSizeCheckerInfo],
    attrs = {
        "size_reports": attr.label_list(
            doc = "size reports that needs to be aggregated",
            providers = [[FuchsiaSizeCheckerInfo]],
            mandatory = True,
        ),
        "_size_report_merger": attr.label(
            default = "//fuchsia/tools:size_report_merger",
            executable = True,
            cfg = "exec",
        ),
    },
)
