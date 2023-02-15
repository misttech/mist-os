# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":providers.bzl", "FuchsiaScrutinyConfigInfo")

def _fuchsia_scrutiny_config_impl(ctx):
    return [
        FuchsiaScrutinyConfigInfo(
            bootfs_files = ctx.files.bootfs_files,
            bootfs_packages = ctx.files.bootfs_packages,
            kernel_cmdline = ctx.files.kernel_cmdline,
            routes_config_golden = ctx.file.routes_config_golden,
            component_resolver_allowlist = ctx.file.component_resolver_allowlist,
            component_route_exceptions = ctx.files.component_route_exceptions,
            component_tree_config = ctx.file.component_tree_config,
            base_packages = ctx.file.base_packages,
            structured_config_policy = ctx.file.structured_config_policy,
        ),
    ]

fuchsia_scrutiny_config = rule(
    doc = """Generates a set of scrutiny configs.""",
    implementation = _fuchsia_scrutiny_config_impl,
    provides = [FuchsiaScrutinyConfigInfo],
    attrs = {
        "bootfs_files": attr.label_list(
            doc = "Set of files expected in bootfs",
            allow_files = True,
        ),
        "bootfs_packages": attr.label_list(
            doc = "Set of packages expected in bootfs",
            allow_files = True,
        ),
        "kernel_cmdline": attr.label_list(
            doc = "Set of cmdline args expected to be passed to the kernel",
            allow_files = True,
        ),
        "routes_config_golden": attr.label(
            doc = "Config file for route resources validation",
            allow_single_file = True,
        ),
        "component_resolver_allowlist": attr.label(
            doc = "Allowlist of components that can be resolved using privileged component resolvers",
            allow_single_file = True,
        ),
        "component_route_exceptions": attr.label_list(
            doc = "Allowlist of all capability routes that are exempt from route checking",
            allow_files = True,
        ),
        "component_tree_config": attr.label(
            doc = "Tree of expected component routes",
            allow_single_file = True,
        ),
        "base_packages": attr.label(
            doc = "Set of base packages expected in the fvm",
            allow_single_file = True,
        ),
        "structured_config_policy": attr.label(
            doc = "File describing the policy of structured config",
            allow_single_file = True,
        ),
    },
)
