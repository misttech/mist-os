"""
Public definitions for the Fuchsia clang toolchain repository.
"""

def register_clang_toolchains(
        # Builidifier proclaims that we should always include a name argument.
        name = ""):  # @unused
    """
    Registers clang as a Bazel toolchain.
    """
    native.register_toolchains(str(Label("//:cc-x86_64")))
    native.register_toolchains(str(Label("//:cc-aarch64")))
