This directory contains support for building the Fuchsia tree
with build actions running on RBE (remote build execution).

# Remote execution wrappers

The top-level remote execution wrappers are used as command prefixes:

*   `cxx_remote_wrapper.py`: prefix wrapper for remote compiling C++
*   `rustc_remote_wrapper.py`: prefix wrapper for remote compiling Rust
    *   Detects and gathers all inputs and tools needed for remote compiling.
    *   Detects extra outputs to download produced by remote compiling.
*   `remote_action.py`: prefix wrapper for generic remote actions
    * Exists as standalone wrapper and library.
    * Includes detailed diagnostics for certain error conditions.
    * Includes limited fault-tolerance and retries.
    * The C++ and Rust wrappers inherit all of the features of this generic
      script.
*   `fuchsia-reproxy-wrap.sh`: automatically start/shutdown `reproxy` (needed by
    `rewrapper`) around any command.  Used by `fx build`.

More details can be found by running with `--help`.

## Support scripts

*   `cl_utils.py`: generic command-line operation library
*   `cxx.py`: for understanding structure of C/C++ compile commands
*   `fuchsia.py`: Fuchsia-tree specific directory layouts and conventions.
    Parties interested in re-using wrapper scripts found here should expect
    to replace this file.
*   `output_leak_scanner.py`: Check that commands and outputs do not leak
    the name of the build output directory, for better caching outcomes.
*   `relativize_args.py`: Attempt to transform commands with absolute paths
    into equivalent commands with relative paths.  This can be useful for
    build systems that insist on absolute paths, like `cmake`.
*   `rustc.py`: for understanding structure of `rustc` compile commands

## Configurations

*   `fuchsia-rewrapper.cfg`: rewrapper configuration
*   `fuchsia-reproxy.cfg`: reproxy configuration

# Troubleshooting tools

*   `action_diff.sh`: recursively queries through two reproxy logs to
    root-cause differences between output and intermediate artifacts.
    This is useful for examining unexpected digest differences and
    cache misses.
*   `bb_fetch_rbe_cas.sh`: retrieve a remote-built artifact from the
    RBE CAS using a buildbucket id and the path under the build output
    directory.
*   `detail-diff.sh`: attempts to compare human-readable representations
    of files, including some binary files, by using tools like `objdump`.
*   `remotetool.sh`: can lookup actions and artifacts in the RBE CAS.
    From: https://github.com/bazelbuild/remote-apis-sdks
*   `reproxy_logs.sh`: subcommand utility
    *   `diff`: report meaningful difference between two reproxy logs
    *   `output_file_digest`: lookup the digest of a remote built artifact
    *   `bandwidth`: report total download and upload bytes from reproxy log

All tools have more detailed usage with `--help`.

# GN files

*   `build/toolchain/rbe.gni`: global `args.gn` variables for RBE

*   `build/toolchain/clang_toolchain.gni`: uses RBE wrappers depending on
    configuration
*   `build/rust/rustc_*.gni`: uses RBE wrappers depending on configuration

# Metrics and logs

*   `upload_reproxy_logs.py`: pushes RBE metrics and detailed logs to
     BigQuery.
     *   This can be enabled by `fx build-metrics`.

*   `pb_message_util.py`: library for translating protobufs to JSON
