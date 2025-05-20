# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This file contains annotations for third-party Rust crates. These are extra
# field values (e.g. rustc_flags, features, deps, etc.), sometimes
# environment-specific, added to default generated values.
#
# These values are intentionally kept in a separate file to keep the main
# BUILD.bazel file simple.

load("@rules_rust//crate_universe:defs.bzl", "crate")

CRATE_ANNOTATIONS = {
    "libc": [
        crate.annotation(
            version = "0.2.171",
            rustc_flags = crate.select(
                common = [
                    "--cfg=libc_priv_mod_use",
                    "--cfg=libc_union",
                    "--cfg=libc_const_size_of",
                    "--cfg=libc_align",
                    "--cfg=libc_core_cvoid",
                    "--cfg=libc_packedN",
                    "--cfg=libc_cfg_target_vendor",
                    "--cfg=libc_int128",
                    "--cfg=libc_non_exhaustive",
                    "--cfg=libc_long_array",
                    "--cfg=libc_ptr_addr_of",
                    "--cfg=libc_underscore_const_names",
                    "--cfg=libc_const_extern_fn",
                ],
                selects = {
                    "@platforms//os:freebsd": [
                        "--cfg=freebsd11",
                    ],
                },
            ),
        ),
    ],
    "nix": [
        crate.annotation(
            version = "0.29.0",
            gen_build_script = False,
            rustc_flags = crate.select(
                common = [],
                selects = {
                    "@platforms//os:linux": [
                        "--cfg=linux",
                        "--cfg=linux_android",
                    ],
                    "@platforms//os:freebsd": [
                        "--cfg=bsd",
                        "--cfg=freebsd",
                        "--cfg=freebsdlike",
                    ],
                    "@platforms//os:macos": [
                        "--cfg=apple_targets",
                        "--cfg=bsd",
                        "--cfg=macos",
                    ],
                },
            ),
        ),
    ],
    "tokio": [
        crate.annotation(
            version = "1.38.1",
            deps = crate.select(
                common = [],
                selects = {
                    "x86_64-unknown-linux-gnu": [
                        "//third_party/rust_crates/vendor/bytes-1.10.0:bytes",
                        "//third_party/rust_crates/vendor/libc-0.2.171:libc",
                        "//third_party/rust_crates/ask2patch/memchr",
                        "//third_party/rust_crates/vendor/mio-0.8.9:mio",
                        "//third_party/rust_crates/vendor/num_cpus-1.16.0:num_cpus",
                        "//third_party/rust_crates/vendor/signal-hook-registry-1.4.1:signal_hook_registry",
                        "//third_party/rust_crates/vendor/socket2-0.5.9:socket2",
                    ],
                },
            ),
            crate_features = crate.select(
                common = [],
                selects = {
                    "x86_64-unknown-linux-gnu": [
                        "bytes",
                        "fs",
                        "io-util",
                        "libc",
                        "mio",
                        "net",
                        "num_cpus",
                        "process",
                        "rt-multi-thread",
                        "rt",
                        "signal",
                        "signal-hook-registry",
                        "socket2",
                        "sync",
                        "time",
                    ],
                },
            ),
            rustc_flags = crate.select(
                common = [],
                selects = {
                    "x86_64-unknown-linux-gnu": [
                        "--cfg=tokio_unstable",
                    ],
                },
            ),
        ),
    ],
    "proc-macro2": [
        crate.annotation(
            version = "1.0.86",
            rustc_flags =
                [
                    "--cfg=span_locations",
                    "--cfg=wrap_proc_macro",
                ],
        ),
    ],
    "zerocopy": [
        crate.annotation(
            version = "0.8.25-alpha.2",
            rustc_flags = [
                "--cfg=zerocopy_core_error_1_81_0",
                "--cfg=zerocopy_diagnostic_on_unimplemented_1_78_0",
                "--cfg=zerocopy_generic_bounds_in_const_fn_1_61_0",
                "--cfg=zerocopy_target_has_atomics_1_60_0",
                "--cfg=zerocopy_aarch64_simd_1_59_0",
                "--cfg=zerocopy_panic_in_const_and_vec_try_reserve_1_57_0",
            ],
        ),
    ],
    "ring": [
        crate.annotation(
            version = "0.17.8",
            # NOTE: Build script of this crate doesn't run due to missing
            # dependency. See https://fxbug.dev/345712835.
            gen_build_script = False,
            deps = [
                "//third_party/rust_crates:ring-core",
            ],
            rustc_env = {
                "RING_CORE_PREFIX": "ring_core_0_17_8_",
            },
        ),
    ],
}
