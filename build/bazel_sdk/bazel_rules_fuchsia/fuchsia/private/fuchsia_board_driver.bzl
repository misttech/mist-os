# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""fuchsia_board_driver() rule."""

load(":fuchsia_package.bzl", "fuchsia_package")
load(":fuchsia_package_resource.bzl", "fuchsia_package_resource")
load(":utils.bzl", "label_name")

def fuchsia_board_driver(
        *,
        name,
        package_name = None,
        archive_name = None,
        platform = None,
        fuchsia_api_level = None,
        components = [],
        visitors = [],
        tools = [],
        subpackages = [],
        resources = [],
        **kwargs):
    # buildifier: disable=function-docstring-args
    """
    A build rule to create a fuchsia_board driver package.

    Args:
        visitors: Devicetree visitors arguments. It will be wrapped into a fuchsia_package_resource target and appended to resources of fuchsia_package target.

    See _fuchsia_package for additional arguments."""
    all_resources = []
    for visitor in visitors:
        label = label_name(visitor) + "_resource"
        dest = "lib/visitors/lib%s.so" % label_name(visitor)
        fuchsia_package_resource(
            name = label,
            src = visitor,
            dest = dest,
        )
        all_resources.append(":" + label)

    all_resources += resources

    fuchsia_package(
        name = name,
        package_name = package_name,
        archive_name = archive_name,
        platform = platform,
        fuchsia_api_level = fuchsia_api_level,
        components = components,
        resources = all_resources,
        tools = tools,
        subpackages = subpackages,
        **kwargs
    )
