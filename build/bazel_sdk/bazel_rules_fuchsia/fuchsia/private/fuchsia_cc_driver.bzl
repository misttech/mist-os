# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""A wrapper around cc_binary to be used for drivers targeting Fuchsia."""

load("//fuchsia/private:fuchsia_cc.bzl", "fuchsia_wrap_cc_binary")

def fuchsia_cc_driver(name, **kwargs):
    """Creates a binary driver which targets Fuchsia.

    Wraps a cc_binary rule and provides appropriate defaults.

    This method currently just simply ensures that libc++ is statically linked.
    In the future it will ensure drivers are correctly versioned and carry the
    appropriate package resources.

    Args:
        name: the target name
        **kwargs: The arguments to forward to cc_binary
    """

    # Grab our
    linkopts = kwargs.pop("linkopts", [])
    linkopts.append("-static-libstdc++")

    # Remove this value because we want to set it on our own. If we don't
    # remove it the fuchsia_wrap_cc_binary to fail with an unknown attribute.
    kwargs.pop("linkshared", None)

    # To forward to fuchsia_wrap_cc_binary. If we don't do this here we end up
    # with duplicate entries in cc_binary which will cause a failure.
    visibility = kwargs.pop("visibility", None)
    tags = kwargs.pop("tags", None)

    native.cc_binary(
        name = name + "_cc_binary",
        linkopts = linkopts,
        linkshared = True,
        visibility = ["//visibility:private"],
        **kwargs,
    )

    fuchsia_wrap_cc_binary(
        name = name,
        bin_name = "lib{}.so".format(name),
        cc_binary = ":{}_cc_binary".format(name),
        exact_cc_binary_deps = kwargs.pop("deps", None),
        visibility = visibility,
        tags = tags,
    )
