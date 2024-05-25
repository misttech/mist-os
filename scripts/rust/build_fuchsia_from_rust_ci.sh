#!/usr/bin/env bash
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

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
# missing libstd, but that's okay because we disable the ELF manifest checker.
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

# Force a rebuild of all Rust targets by providing a unique version string each time.
version_string="$(date '+%Y/%m/%d %H:%M:%S')"

set -x

# Disabling debuginfo speeds up the build by about 8%.
$fx set \
    --no-goma \
    --args "rustc_prefix = \"$rust_prefix\"" \
    --args "rustc_version_string = \"$version_string\"" \
    --args 'rust_cap_lints = "warn"' \
    --args 'disable_elf_checks = true' \
    --args 'rustc_use_response_files = false' \
    --args 'rust_one_rlib_per_dir = false' \
    --args 'restat_rust = false' \
    --args 'verify_depfile = false' \
    --args 'debuginfo = "none"' \
    --with '//bundles/buildbot/minimal' \
    workbench_eng.x64 \
    ; echo

set +e
time $fx clippy --all --quiet
retcode=$?
set -e

set +x

print_banner

exit $retcode
