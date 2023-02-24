"""
Public definitions for the Fuchsia clang toolchain repository.
"""

load("@fuchsia_sdk//:generated_constants.bzl", "constants")

def register_clang_toolchains(
        # Builidifier proclaims that we should always include a name argument.
        name = ""):  # @unused
    # buildifier: disable=function-docstring-args
    """
    Registers clang as a Bazel toolchain.
    """
    if "x64" in constants.target_cpus:
        native.register_toolchains(str(Label("//:cc-x86_64")))
    if "arm64" in constants.target_cpus:
        native.register_toolchains(str(Label("//:cc-aarch64")))
