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
                        "//third_party/rust_crates/vendor/libc-0.2.169:libc",
                        "//third_party/rust_crates/ask2patch/memchr",
                        "//third_party/rust_crates/vendor/mio-0.8.9:mio",
                        "//third_party/rust_crates/vendor/num_cpus-1.16.0:num_cpus",
                        "//third_party/rust_crates/vendor/signal-hook-registry-1.4.1:signal_hook_registry",
                        "//third_party/rust_crates/vendor/socket2-0.5.5:socket2",
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
                common = [
                ],
                selects = {
                    "x86_64-unknown-linux-gnu": [
                        "--cfg=tokio_unstable",
                    ],
                },
            ),
        ),
    ],
}
