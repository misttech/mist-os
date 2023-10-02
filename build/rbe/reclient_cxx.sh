#!/bin/bash
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Bare-minimal execution of rewrapper, with no additional logic or features,
# purely for the sake of performance with minimal overhead.
# Intended for clang and gcc.

# Avoid invoking any subprocesses, use shell built-in equivalents where possible.

# Requires:
#   reproxy is already running

readonly script="$0"
readonly script_dir="${script%/*}"
readonly script_basename="${script##*/}"

# not normalized
readonly exec_root="$script_dir/../.."

# OSTYPE and HOSTTYPE are always set by bash
case "$OSTYPE" in
  linux*) _HOST_OS=linux ;;
  darwin*) _HOST_OS=mac ;;
  *) echo >&2 "Unknown OS: $OSTYPE" ; exit 1 ;;
esac
case "$HOSTTYPE" in
  x86_64) _HOST_ARCH=x64 ;;
  arm64) _HOST_ARCH=arm64 ;;
  *) echo >&2 "Unknown ARCH: $HOSTTYPE" ; exit 1 ;;
esac

readonly HOST_PLATFORM="$_HOST_OS-$_HOST_ARCH"

readonly reclient_bindir="$exec_root/prebuilt/proprietary/third_party/reclient/$HOST_PLATFORM"
readonly rewrapper="$reclient_bindir"/rewrapper
readonly cfg="$script_dir"/fuchsia-rewrapper.cfg
readonly reproxy_wrap="$script_dir"/fuchsia-reproxy-wrap.sh

function dmsg() {
  # Uncomment to debug:
  # echo "[$script_basename]" "$@"
  :
}

if [[ -z "$RBE_server_address" ]]
then
  # Uncommon case: manual repro.  Automatically start/shutdown reproxy.
  dmsg "No env RBE_server_address found.  Re-launching with $reproxy_wrap"
  exec "$reproxy_wrap" -- "$0" "$@"
  # no return
else
  # Common case: already inside `fx build`
  dmsg "Using RBE_server_address=$RBE_server_address"
fi

if [[ "$#" == 0 ]]
then exit
fi

# Separate rewrapper options from the command, based on '--'.
rewrapper_opts=()
for opt
do
  case "$opt" in
    --) shift; break ;;
    *) rewrapper_opts+=( "$opt" ) ;;
  esac
  shift
done
# Everything else is the compiler command.
compile_cmd=( "$@" )

if [[ "${#compile_cmd[@]}" == 0 ]]
then exit
fi

# Detect cases that are unsupported by re-client.
local_only=0
for opt in "${compile_cmd[@]}"
do
  case "$opt" in
    *.S) local_only=1 ;;  # b/220030106: no plan to support asm preprocessing.
  esac
done

cmd_prefix=()
if [[ "$local_only" == 0 ]]
then
  cmd_prefix+=(
    # RBE_v=3  # for verbose logging
    "$rewrapper"
    --exec_root "$exec_root"
    --cfg "$cfg"

    # for C++
    --labels=type=compile,compiler=clang,lang=cpp
    --canonicalize_working_dir=true

    "${rewrapper_opts[@]}"
    --
  )
fi

# set -x
exec "${cmd_prefix[@]}" "${compile_cmd[@]}"
