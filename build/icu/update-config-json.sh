#!/bin/bash
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -e

# Either update or check the content of //build/uci/jiri_generated/config.json
# NOTE: This is called by a Jiri hook!

_SCRIPT_DIR="$(dirname "${BASH_SOURCE[0]}")"
_DEFAULT_OUTPUT="${_SCRIPT_DIR}/jiri_generated/config.json"

die () {
  echo >&2 "ERROR: $*"
  return 1
}

function usage {
  cat <<EOF
Usage: ${_BASH_SOURCE[0]} [options]

Compute the ICU config.json file from the current state of the checkout.
Valid options:

  --fuchsia-dir=DIR  Specify Fuchsia source directory. Default is current dir.

  --output=FILE   Write result to a file. Default is ${_DEFAULT_OUTPUT}

  --mode=write    Write the generated config file to output file (default).

  --mode=print    Print the generated config file instead of writing it
                  to the output file.

  --mode=check    Check that the generated config file matches the current
                  output file content.

  --timestamp=FILE Touch FILE if this command runs succesfully.
EOF
  return 1
}

# Obtains the commit ID of the current branch for the provided repository
# directory.
#
# Echoes either a valid commit ID, or a string starting with `not_found`.
#
# Args:
#   ${1}: the path to the git repository to examine (must contain the .git dir)
function get_git_commit_id() {
  local _git_path="${1}"
  GIT_CONFIG_NOSYSTEM=1 TZ=UTC \
    git --no-optional-locks --git-dir="${_git_path}/.git" rev-parse HEAD
}


output_file="${_DEFAULT_OUTPUT}"
fuchsia_dir=.
timestamp_file=
mode="write"
for OPT; do
  case "$OPT" in
    --output=*)
      output_file="${OPT#--*=}"
      ;;
    --mode=*)
      mode="${OPT#--*=}"
      ;;
    --timestamp=*)
      timestamp_file="${OPT#--*=}"
      ;;
    --fuchsia-dir=*)
      fuchsia_dir="${OPT#--*=}"
      ;;
    --help)
      usage
      ;;
    -*)
      die "Invalid option $OPT, see --help."
      ;;
    *)
      die "This script does not take arguments, see --help."
      ;;
  esac
done

icu_default_dir="${fuchsia_dir}/third_party/icu/default"
icu_latest_dir="${fuchsia_dir}/third_party/icu/latest"

[[ -d "$icu_default_dir" ]] || die "Missing directory: ${icu_default_dir}"
[[ -d "$icu_latest_dir" ]] || die "Missing directory: ${icu_latest_dir}"

default_commit_id="$(get_git_commit_id "${icu_default_dir}")" ||
    die "commit id not found in $icu_default_dir"

latest_commit_id="$(get_git_commit_id "${icu_latest_dir}")" ||
    die "commit id not found in $icu_latest_dir"

# Compute the new JSON content.
content=$(cat <<EOF
{
  "default": "${default_commit_id}",
  "latest": "${latest_commit_id}"
}
EOF
)

case "$mode" in
  write)
    printf "%s" "${content}" > "${output_file}"
    ;;
  print)
    printf "%s" "${content}"
    ;;
  check)
    cur_content="$(cat "${output_file}")" || die "Cannot read: ${output_file}"
    if [[ "${cur_content}" != "${content}" ]]; then
      echo >&2 "Output file ${output_file} not up-to-date!"
      diff -burN "${output_file}" <(printf %s "$content") >&2 || true
      cat >&2 <<EOF

This means the //third_party/icu/{default,latest} git HEADs have changed since
your last 'jiri update'. Please invoke the following script manually to fix this:

${BASH_SOURCE[0]}

EOF
      exit 1
    fi
    ;;
  *)
    die "Invalid --mode=${mode} value, must be one of: check, print, write"
    ;;
esac

if [[ -n "${timestamp_file}" ]]; then
  touch "${timestamp_file}"
fi
