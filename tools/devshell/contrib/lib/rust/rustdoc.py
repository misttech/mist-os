#!/usr/bin/env fuchsia-vendored-python
# Copyright 2018 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

### generates documentation for a Rust target

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

import rust
from rust import HOST_PLATFORM, ROOT_PATH


def manifest_path_from_path_or_gn_target(arg):
    if arg.endswith("Cargo.toml"):
        return Path(arg)
    else:
        gn_target = rust.GnTarget(arg)
        if not str(gn_target).startswith("//third_party"):
            gn_target.label_name += ".actual"
        return gn_target.manifest_path()


def main():
    parser = argparse.ArgumentParser("fx rustdoc")
    parser.add_argument(
        "manifest_path",
        metavar="gn_target",
        type=manifest_path_from_path_or_gn_target,
        help="GN target to document. Use '.[:target]' to discover the cargo \
                target for the current directory or use the absolute path to \
                the target (relative to $FUCHSIA_DIR). For example: \
                //garnet/bin/foo/bar:baz. Alternatively, this can be a path \
                to a Cargo.toml file of a package for which to generate docs.",
    )
    parser.add_argument(
        "--target", help="Target triple for which this crate is being compiled")
    parser.add_argument("--out-dir", help="Path to the Fuchsia build directory")
    parser.add_argument(
        "--no-deps",
        action="store_true",
        help="Disable building of docs for dependencies",
    )
    parser.add_argument(
        "--doc-private", action="store_true", help="Document private items")
    parser.add_argument(
        "--open", action="store_true", help="Open the generated documentation")

    args = parser.parse_args()

    if args.out_dir:
        build_dir = args.out_dir
    else:
        build_dir = os.environ["FUCHSIA_BUILD_DIR"]

    rust_dir = ROOT_PATH / "prebuilt/third_party/rust" / HOST_PLATFORM / "bin"
    buildtools_dir = ROOT_PATH / "prebuilt/third_party"
    clang_prefix = buildtools_dir / "clang" / HOST_PLATFORM / "bin"
    clang = str(clang_prefix / "clang")
    shared_libs_root = ROOT_PATH / build_dir
    fuchsia_sysroot = (
        ROOT_PATH / build_dir /
        "zircon_toolchain/obj/zircon/public/sysroot/sysroot")
    sysroot = buildtools_dir / "sysroot" / "linux"

    env = os.environ.copy()

    for target in (
            "X86_64_APPLE_DARWIN",
            "X86_64_UNKNOWN_LINUX_GNU",
            "X86_64_FUCHSIA",
            "AARCH64_FUCHSIA",
    ):
        env[f"CARGO_TARGET_{target}_LINKER"] = clang
        if "FUCHSIA" in target:
            env[f"CARGO_TARGET_{target}_RUSTFLAGS"] = f"-Clink-arg=--sysroot={fuchsia_sysroot} -Lnative={shared_libs_root}"
        if "LINUX" in target:
            env[f"CARGO_TARGET_{target}_RUSTFLAGS"] = f"-Clink-arg=--sysroot={sysroot}"
    env["CC"] = clang
    env["CXX"] = str(clang_prefix / "clang++")
    env["AR"] = str(clang_prefix / "llvm-ar")
    env["RANLIB"] = str(clang_prefix / "llvm-ranlib")
    env["RUSTC"] = str(rust_dir / "rustc")
    env["RUSTDOC"] = str(
        ROOT_PATH / "scripts/rust/rustdoc_no_ld_library_path.sh")
    env["RUSTDOCFLAGS"] = "-Z unstable-options --enable-index-page"
    env["RUST_BACKTRACE"] = "1"
    # Ideally this would somehow be automatically handled by the Cargo.toml
    # generator reading the gn BUILD config. It doesn't do that today because
    # we're re-using the third_party Cargo manifests, so we hardcode it instead
    with open(ROOT_PATH / "third_party/icu/default/version.json") as f:
        env["RUST_ICU_MAJOR_VERSION_NUMBER"] = json.load(f)["major_version"]

    call_args = [
        rust_dir / "cargo", "doc", "--manifest-path=" + str(args.manifest_path)
    ]

    if args.target:
        call_args.append("--target=" + args.target)

    if args.no_deps:
        call_args.append("--no-deps")

    if args.open:
        call_args.append("--open")

    if args.doc_private:
        call_args.append("--document-private-items")

    # rustdoc doesn't emit dep-info, so cargo doc uses a heuristic to detect
    # when it needs to rebuild: if any file next to or under the Cargo.toml
    # are more recent than the output, it re-runs rustdoc. This is an issue
    # for us because our cargo manifests are in the out dir, and don't
    # actually have the crate root next to them. We do the pessimal thing
    # and unconditionally touch a file next to the manifest to force it to
    # re-run rustdoc every time.
    # TODO: once https://github.com/rust-lang/cargo/issues/12266 is resolved this
    # logic can be removed.
    stamp = args.manifest_path.parent / "docs_input_stamp"
    stamp.touch()

    # run cargo from third_party/rust_crates which has an appropriate .cargo/config
    return subprocess.call(
        call_args, env=env, cwd=ROOT_PATH / "third_party/rust_crates")


if __name__ == "__main__":
    sys.exit(main())
