#!/usr/bin/env bash
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script is run from the Rust Project CI to build Fuchsia as a way of
# catching regressions. If you are a Rust developer, see the comments toward
# the end of this file for more information about how the Fuchsia build works
# and how to customize it. More documentation can be found in the Rustc
# Developer Guide.

set -eu -o pipefail

print_banner() {
  {
    git_commit=$(git rev-parse HEAD)
    echo
    echo "###############################################################################"
    echo "#                                                                             #"
    echo "#  This check builds the Fuchsia operating system.                            #"
    echo "#                                                                             #"
    echo "#  Most code is built in 'check mode' using clippy, to maximize coverage in   #"
    echo "#  the compiler front end while reducing build times.                         #"
    echo "#                                                                             #"
    echo "#  You can browse the Fuchsia source code at the following URL:               #"
    echo "#  https://cs.opensource.google/fuchsia/fuchsia/+/$git_commit:"
    echo "#                                                                             #"
    echo "###############################################################################"
    echo
  } >&2
}

# toabs converts the possibly relative argument into an absolute path. Run in a
# subshell to avoid changing the caller's working directory.
toabs() (
  cd $(dirname $1)
  echo ${PWD}/$(basename $1)
)

fuchsia=$(toabs $(dirname $0)/../..)
fx=$fuchsia/.jiri_root/bin/fx

if ! [ -d $RUST_INSTALL_DIR ]; then
    echo "RUST_INSTALL_DIR must be set to a valid value: $RUST_INSTALL_DIR"
    exit 1
fi

rust_prefix=$RUST_INSTALL_DIR

# Stub out rustfmt.
cat <<END >$rust_prefix/bin/rustfmt
#!/usr/bin/env bash
cat
END
chmod +x $rust_prefix/bin/rustfmt

# Stub out runtime.json. This will cause the build to produce invalid packages
# missing libstd, but that's okay because we don't run an emulator anyway, and
# we disable the ELF manifest checker in the build.
cat <<END >$rust_prefix/lib/runtime.json
[
  {
    "runtime": [],
    "rustflags": [],
    "target": [
      "aarch64-unknown-fuchsia"
    ]
  },
  {
    "runtime": [],
    "rustflags": [],
    "target": [
      "x86_64-unknown-fuchsia"
    ]
  },
  {
    "runtime": [
    ],
    "rustflags": [
      "-Cprefer-dynamic"
    ],
    "target": [
      "aarch64-unknown-fuchsia"
    ]
  },
  {
    "runtime": [
    ],
    "rustflags": [
      "-Cprefer-dynamic"
    ],
    "target": [
      "x86_64-unknown-fuchsia"
    ]
  }
]
END

$fx metrics disable

print_banner

# Detect Rust toolchain changes by hashing the entire toolchain.
version_string="$(find prebuilt/third_party/rust/linux-x64 -type f -print0 | sort -z | xargs -0 sha1sum | sha1sum)"

set -x

# Here are the build arguments used when building Fuchsia.
#
# These wire up the compiler from CI and customize the build for the Rust CI
# environment in a few ways:
#
# - Don't fail the build because of new warnings, since we don't use the pinned
#   compiler version.
# - Disable debuginfo, which speeds up the build by about 8%.
# - Disable some rustc wrapper scripts that perform unnecessary build checks.
# - Build the bundle of unit tests called "minimal".
#
# `fx set` creates a file called `out/default/args.gn` with the build arguments
# and then runs GN, Fuchsia's meta-build system which generates ninja files. You
# may also modify args.gn directly and rerun the build, which will pick up the
# changes automatically.
$fx set \
    --args "rustc_prefix = \"$rust_prefix\"" \
    --args "rustc_version_string = \"$version_string\"" \
    --args 'rust_cap_lints = "warn"' \
    --args 'rustc_use_response_files = false' \
    --args 'rust_one_rlib_per_dir = false' \
    --args 'restat_rust = false' \
    --args 'verify_depfile = false' \
    --args 'debuginfo = "none"' \
    --args 'disable_elf_checks = true' \
    --with '//bundles/buildbot/minimal' \
    workbench_eng.x64 \
    ; echo

# Now run the build. We use `fx clippy` to drive the build because it reduces the
# amount of Rust code that we actually need to produce binaries for. Under the
# hood `fx clippy` runs ninja.
set +e
time $fx clippy --all
retcode=$?
set -e

set +x

print_banner

exit $retcode
