#!/bin/bash
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Bare-minimal execution of rewrapper, with no additional logic or features,
# purely for the sake of performance with minimal overhead.
# Intended for clang and gcc.

# Avoid invoking any subprocesses, use shell built-in equivalents where possible.

readonly script="$0"
# assume script is always with path prefix, e.g. "./$script"
readonly script_dir="${script%/*}"  # dirname
readonly script_basename="${script##*/}"  # basename

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

# rewrapper expects input paths to be named relative to the exec_root
# and not the working directory.
# Pass this in from GN/ninja to avoid calculating relpath here.
working_subdir=

# Separate rewrapper options from the command, based on '--'.
rewrapper_opts=()
for opt
do
  # Extract optarg from --opt=optarg
  optarg=
  case "$opt" in
    -*=*) optarg="${opt#*=}" ;;  # remove-prefix, shortest-match
  esac
  case "$opt" in
    --working-subdir=*) working_subdir="$optarg" ;;
    --) shift; break ;;
    *) rewrapper_opts+=( "$opt" ) ;;
  esac
  shift
done

[[ -n "$working_subdir" ]] || { msg "Missing required option --working-subdir." ; exit 1 ;}

# Everything else is the compiler command.
compile_cmd=( "$@" )

if [[ "${#compile_cmd[@]}" == 0 ]]
then exit
fi

# Assume first token is the compiler (which is what rewrapper does).
compiler="${compile_cmd[0]}"
# Infer language from compiler.
case "$compiler" in
  *++*) lang=cxx ;;
  *) lang=c ;;
esac

# Detect cases that are unsupported by re-client.
local_only=0
save_temps=0
output=
crash_diagnostics_dir=

prev_opt=
for opt in "${compile_cmd[@]}"
do
  # handle --option arg
  if [[ -n "$prev_opt" ]]
  then
    eval "$prev_opt"=\$opt
    prev_opt=
    continue
  fi

  # Extract optarg from --opt=optarg
  optarg=
  case "$opt" in
    -*=*) optarg="${opt#*=}" ;;  # remove-prefix, shortest-match
  esac

  case "$opt" in
    -save-temps | --save-temps) save_temps=1 ;;
    -fcrash-diagnostics-dir) prev_opt=crash_diagnostics_dir ;;
    -fcrash-diagnostics-dir=*) crash_diagnostics_dir="$optarg" ;;
    -o) prev_opt=output ;;
    *.S) local_only=1 ;;  # b/220030106: no plan to support asm preprocessing.
  esac
done

if [[ "$local_only" == 1 ]]
then
  exec "${compile_cmd[@]}"
  # no return
fi

[[ -n "$output" ]] || { msg "Missing required compiler option -o" ; exit 1 ;}

# Paths must be relative to exec_root.
remote_input_files=()
remote_output_files=()
remote_output_dirs=()

# TODO(b/302613832): delete this once upstream supports it.
if [[ "$save_temps" == 1 ]]
then
  temp_base="${output##*/}"  # remove paths
  temp_base="${temp_base%%.*}"  # remove all extensions (like ".cc.o")
  temp_exts=( .bc .s )
  if [[ "$lang" == cxx ]]
  then temp_exts+=( .ii )
  else temp_exts+=( .i )
  fi
  for ext in "${temp_exts[@]}"
  do remote_output_files+=( "$working_subdir/$temp_base$ext" )
  done
fi

# TODO(b/272865494): delete this once upstream supports it.
if [[ -n "$crash_diagnostics_dir" ]]
then remote_output_dirs+=( "$working_subdir/$crash_diagnostics_dir" )
fi

remote_input_files_opt=()
if [[ "${#remote_input_files[@]}" > 0 ]]
then
  # This is an uncommon case and thus allowed to be slow.
  remote_input_files_opt+=( "--inputs=$(IFS=, ; echo "${remote_input_files[*]}")" )
fi

remote_output_files_opt=()
if [[ "${#remote_output_files[@]}" > 0 ]]
then
  # This is an uncommon case and thus allowed to be slow.
  remote_output_files_opt+=( "--output_files=$(IFS=, ; echo "${remote_output_files[*]}")" )
fi

remote_output_dirs_opt=()
if [[ "${#remote_output_dirs[@]}" > 0 ]]
then
  # This is an uncommon case and thus allowed to be slow.
  remote_output_dirs_opt+=( "--output_directories=$(IFS=, ; echo "${remote_output_dirs[*]}")" )
fi

# Set the rewrapper options.
cmd_prefix+=(
  # RBE_v=3  # for verbose logging
  "$rewrapper"
  --exec_root "$exec_root"
  --cfg "$cfg"

  # for C++
  --labels=type=compile,compiler=clang,lang=cpp
  --canonicalize_working_dir=true

  "${remote_input_files_opt[@]}"
  "${remote_output_files_opt[@]}"
  "${remote_output_dirs_opt[@]}"

  "${rewrapper_opts[@]}"
  --
)

# set -x
exec "${cmd_prefix[@]}" "${compile_cmd[@]}"
