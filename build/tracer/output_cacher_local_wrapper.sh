#!/bin/bash
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script expects args in the form of three command slices, separated by --:
#   outer_wrapper ... -- RBE_wrapper ... -- command ...
#
# This writes the outer_wrapper command prefix into a temporary file
# and passes that through --local_wrapper to the RBE_wrapper (which accepts
# rewrapper options).  --local_wrapper is limited to a single shell token.
# This script could have been implemented pure using shell syntax however,
# to make it composable with other out wrapper (like the action tracer),
# we hide all logic and syntax inside this script.

# The first command slice is the outer wrapper.
# An example of such a wrapper is build/tracer/output_cacher.py.
outer_wrapper_command=()
for arg
do
  case "$arg" in
    --) shift; break ;;
    *) outer_wrapper_command+=( "$arg" ) ;;
  esac
  shift
done

# The second command slice is an rewrapper-like command.
# This could be `rewrapper` itself, or another wrapper script like
# `fuchsia-rbe-action.sh`, `rustc-remote-wrapper.sh`,
# `cxx-remote-wrapper.sh`.
rewrapper_command_prefix=()

for arg
do
  case "$arg" in
    --) shift; break ;;
    *) rewrapper_command_prefix+=( "$arg" ) ;;
  esac
  shift
done

primary_command=("$@")

# Find a suitable temporary file name, based on declared outputs.
# output_cacher.py --outputs takes repeated arguments until the next flag.
looking_at_output=0
output_candidates=()
for arg in "${outer_wrapper_command[@]}"
do
  case "$arg" in
    --outputs) looking_at_output=1 ;;
    -- | --*) looking_at_output=0 ;;
    *) if [[ "$looking_at_output" == 1 ]] ; then
      output_candidates+=( "$arg" )
      fi
      ;;
  esac
done

output="${output_candidates[0]}"
# Extract "FILENAME" from "substitute_after:flag:FILENAME".
case "$output" in
  substitute_after:*) output="${output#substitute_after:*:}" ;;
esac


# Write the outer_wrapper_command into a file so that it can be
# invoked by rewrapper --local_wrapper as a single shell token.
# This is equivalent to forwarding all tokens to the outer wrapper.
# @Q protects tokens with spaces by single-quoting them.
local_wrapper="$output".wrap.sh
cat > "$local_wrapper" <<EOF
#!/bin/sh
${outer_wrapper_command[@]@Q} -- "\$@"
EOF
chmod +x "$local_wrapper"

function cleanup() {
  rm -f "$local_wrapper"
}

trap cleanup EXIT

# Prefix the local-wrapper argument with a ./ to avoid looking in PATH.
"${rewrapper_command_prefix[@]}" --local_wrapper=./"$local_wrapper" -- "${primary_command[@]}"
