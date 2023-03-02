#! /bin/bash
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -eu

# Shows the current git commit ID for the given git path.
# A single invocation takes about 7ms to complete.
#
# Args:
#   - ${1}: format ("json"|"bzl");
#   - ${2}, ${3}, ${4}: the directories to examine, in
#      order: default, stable, latest.


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
    git --no-optional-locks --git-dir="${_git_path}/.git" rev-parse HEAD \
      || echo "not_found/${_git_path}"
}

readonly format="${1}"
shift

if [[ "${format}" != "json" && "${format}" != "bzl" ]]; then
  echo "$(basename ${0}): format must be either 'json' or 'bzl', was: ${format}"
  exit 1
fi

# Filesystem path to //third_party/icu/default
readonly default_git_path="${1}"
readonly default_commit_id="$(get_git_commit_id ${default_git_path})"
if [[ "${default_commit_id}" == "" ]]; then
  echo "default commit id not found"
  exit 1
fi

# Filesystem path to //third_party/icu/stable
readonly stable_git_path="${2}"
readonly stable_commit_id="$(get_git_commit_id ${stable_git_path})"
if [[ "${stable_commit_id}" == "" ]]; then
  echo "stable commit id not found"
  exit 1
fi

# Filesystem path to //third_party/icu/latest
readonly latest_git_path="${3}"
readonly latest_commit_id="$(get_git_commit_id ${latest_git_path})"
if [[ "${latest_commit_id}" == "" ]]; then
  echo "latest commit id not found"
  exit 1
fi


if [[ "${format}" == "json" ]]; then
  # Echo the result in JSON format, useful for GN and third party tools.
  readonly output="$(cat <<EOF
{
  "default": "${default_commit_id}",
  "stable": "${stable_commit_id}",
  "latest": "${latest_commit_id}"
}
EOF
)"
  echo "${output}"
elif [[ "${format}" == "bzl" ]]; then
  # Echo the result in bazel's starlark format.
  readonly output="$(cat <<EOF
# AUTO_GENERATED - DO NOT EDIT!

icu_flavors = struct(
    default_git_commit = "${default_commit_id}",
    latest_git_commit = "${latest_commit_id}",
    stable_git_commit = "${stable_commit_id}",
)
EOF
)"
  echo "${output}"
else
  echo "$(basename ${0}): unknown format: ${format}"
  exit 1
fi
