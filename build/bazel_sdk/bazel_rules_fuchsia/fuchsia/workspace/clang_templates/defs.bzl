"""
Public definitions for the Fuchsia clang toolchain repository.
"""

load("@fuchsia_sdk//:generated_constants.bzl", "constants")

_TOOLCHAINS_MAP = {
    "x64": "//:cc-x86_64",
    "arm64": "//:cc-aarch64",
    "riscv64": "//:cc-riscv64",
}

def register_clang_toolchains(
        # Builidifier proclaims that we should always include a name argument.
        name = ""):  # @unused
    # buildifier: disable=function-docstring-args
    """
    Registers clang as a Bazel toolchain.
    """
    for cpu in constants.target_cpus:
        native.register_toolchains(str(Label(_TOOLCHAINS_MAP[cpu])))
