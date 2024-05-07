# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""A wrapper around cc_binary to be used for drivers targeting Fuchsia."""

load("//fuchsia/private:fuchsia_cc.bzl", "fuchsia_wrap_cc_binary")

_MISSING_SRCS_FAIL_MESSAGE = """fuchsia_cc_requires at least 1 src which calls FUCHSIA_DRIVER_EXPORT.

Once we are able to migrate away from cc_binary -> cc_shared_library we be able to remove this
restriction but until then we must include that src to properly link in the required
symbols.
"""

def fuchsia_cc_driver(name, srcs = [], output_name = None, deps = [], **kwargs):
    """Creates a binary driver which targets Fuchsia.

    Wraps a cc_binary rule and provides appropriate defaults.

    This method currently just simply ensures that libc++ is statically linked.
    In the future it will ensure drivers are correctly versioned and carry the
    appropriate package resources.

    Args:
        name: the target name
        srcs: the sources to include in the driver.
        output_name: (optional) the name of the .so to build. If excluded will default to lib<name>.so
        deps: The deps for the driver
        **kwargs: The arguments to forward to cc_binary
    """
    if len(srcs) == 0:
        fail(_MISSING_SRCS_FAIL_MESSAGE)

    # Ensure that our binary is named with a .so at the end
    bin_name = output_name or "lib{}".format(name)
    if not bin_name.endswith(".so"):
        bin_name = bin_name + ".so"

    # Grab the user supplied linkopts and add our specific opts that are required
    # for all drivers
    linkopts = kwargs.pop("linkopts", [])
    linkopts.extend([
        # We need to run our own linker script to limit the symbols that are exported
        # and to make the driver framework symbols global.
        "-Wl,--undefined-version",
        "-Wl,--version-script",
        "$(location @fuchsia_sdk//fuchsia/private:driver.ld)",
    ])

    # Remove this value because we want to set it on our own. If we don't
    # remove it the fuchsia_wrap_cc_binary to fail with an unknown attribute.
    kwargs.pop("linkshared", None)

    # To forward to fuchsia_wrap_cc_binary. If we don't do this here we end up
    # with duplicate entries in cc_binary which will cause a failure.
    visibility = kwargs.pop("visibility", None)
    tags = kwargs.pop("tags", None)
    testonly = kwargs.pop("testonly", None)

    # Ensure we are packaging the lib/libdriver_runtime.so
    deps.append(
        "@fuchsia_sdk//pkg/driver_runtime_shared_lib",
    )

    native.cc_binary(
        name = name + "_cc_binary",
        additional_linker_inputs = [
            "@fuchsia_sdk//fuchsia/private:driver.ld",
        ],
        linkopts = linkopts,
        linkshared = True,
        srcs = srcs,
        deps = deps,
        features = kwargs.pop("features", []) + [
            # Ensure that we are statically linking c++.
            "static_cpp_standard_library",
        ],
        visibility = ["//visibility:private"],
        testonly = testonly,
        **kwargs
    )

    fuchsia_wrap_cc_binary(
        name = name,
        # Ensure that our bin_name ends in .so
        bin_name = bin_name,
        install_root = "driver/",
        cc_binary = ":{}_cc_binary".format(name),
        exact_cc_binary_deps = deps,
        # We do not want libc++ and libunwind packaged since they get statically linked.
        package_clang_dist_files = False,
        visibility = visibility,
        testonly = testonly,
        tags = tags,
    )
