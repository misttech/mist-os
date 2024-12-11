# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Creates a fuchsia_package_resource for devicetree visitor."""

load(
    "//fuchsia/private:fuchsia_package_resource.bzl",
    "fuchsia_package_resource",
)

def fuchsia_devicetree_visitor(name, output_name = None, additional_linker_inputs = [], user_link_flags = [], **kwargs):
    """Creates a fuchsia_package_resource for devicetree visitor.

    Args:
        name: the target name
        output_name: (optional) the name of the .so to be included into the package. If excluded will default to <name>.so
        user_link_flags: this will be propagaed to cc_shared_library for any additional flags that you may want to pass to the linker
        additional_linker_inputs: this will be propagaed to cc_shared_library for any additional files that you may want to pass to the linker, for example, linker scripts
        **kwargs: The arguments to forward to cc_library
    """
    lib_name = "{}_lib".format(name)
    visibility = kwargs.pop("visibility", None)

    native.cc_library(
        name = lib_name,
        **kwargs
    )
    additional_linker_inputs = kwargs.pop("additional_linker_inputs", [])
    user_link_flags = kwargs.pop("user_link_flags", [])

    # Compute the label of the linker script.
    #
    # NOTE: A value of //fuchsia/private:driver.ld will not work here, as it will
    # be interpreted as a package 'fucshia' or the project's workspace itself.
    #
    # Using @rules_fuchsia//fuchsia/private:driver.ld would break client workspaces
    # that still use a standalone @fuchsia_sdk repository.
    visitor_ld_target = "@fuchsia_sdk//fuchsia/private:visitor.ld"

    user_link_flags.extend([
        "-Wl,--version-script",
        "$(location %s)" % visitor_ld_target,
    ])
    additional_linker_inputs.append(visitor_ld_target)

    shared_lib_name = "{}_shared_lib".format(name)
    native.cc_shared_library(
        name = shared_lib_name,
        deps = [
            ":" + lib_name,
        ],
        additional_linker_inputs = additional_linker_inputs,
        user_link_flags = user_link_flags,
    )

    resource_name = name
    dest = "lib/visitors/{}.so".format(output_name or name)
    fuchsia_package_resource(
        name = resource_name,
        src = ":" + shared_lib_name,
        dest = dest,
        visibility = visibility,
    )
