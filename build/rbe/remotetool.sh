#!/bin/bash
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# See https://github.com/bazelbuild/remote-apis-sdks
# `remotetool` can be used to inspect past remote actions, re-execute them,
# fetch artifacts, etc.

set -uo pipefail

script="$0"
script_dir="$(dirname "$script")"

# defaults
config="$script_dir"/fuchsia-reproxy.cfg

source "$script_dir"/common-setup.sh

project_root="$(readlink -f "$script_dir"/../..)"
project_root_rel="$(relpath . "$project_root")"

# Assume there is only the host platform tool under prebuilt
remotetool="$project_root_rel"/prebuilt/third_party/remote-apis-sdks/"$HOST_PLATFORM"/remotetool
test -x "$remotetool" || {
  echo "'remotetool' not found in prebuilts at $remotetool"
  exit 1
}

usage() {
  cat <<EOF
$script [remotetool options]

This script wraps around the 'remotetool' tool for convenience, by fixing
some common options.

remotetool --help:
EOF
  "$remotetool" --help
}

remotetool_args=()

prev_opt=
# Extract script options before --
for opt
do
  # handle --option arg
  if test -n "$prev_opt"
  then
    eval "$prev_opt"=\$opt
    prev_opt=
    shift
    continue
  fi

  # Extract optarg from --opt=optarg
  optarg=
  case "$opt" in
    -*=*) optarg="${opt#*=}" ;;  # remove-prefix, shortest-match
  esac

  case "$opt" in
    --help|-h) usage; exit;;
    # Forward all other options to remotetool
    *) remotetool_args+=("$opt") ;;
  esac
  shift
done
test -z "$prev_opt" || { echo "Option is missing argument to set $prev_opt." ; exit 1;}

# Grab RBE action parameters from the reproxy config file.
service="$(grep "^service=" "$config" | cut -d= -f2)"
instance="$(grep "^instance=" "$config" | cut -d= -f2)"

auto_args=(
  --service "$service"
  --instance "$instance"
)
if grep -q "^use_application_default_credentials=true" "$config"
then auto_args+=( --use_application_default_credentials )
fi

full_command=( "$remotetool" "${auto_args[@]}" "${remotetool_args[@]}" )
echo "${full_command[@]}"
exec "${full_command[@]}"
