#!/bin/bash

# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

function usage() {
  cat <<EOF
$0 [options] -- clippy-driver-arguments...

Options:
  --help | -h : print help and exit
  --output FILE : clippy file to output (required)
  --jq FILE : path to 'jq' (required)
  --fail : clippy cause failure
  --quiet : produce output without printing or failing

EOF
}

output=
jq=
driver_options=()
fail=0
quiet=0

# Extract options before --
prev_opt=
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
  case "$opt" in
    -*=*) optarg="${opt#*=}" ;;  # remove-prefix, shortest-match
  esac
  case "$opt" in
    --help|-h) usage ; exit ;;
    --output) prev_opt=output ;;
    --output=*) output="$optarg" ;;
    --jq) prev_opt=jq ;;
    --jq=*) jq="$optarg" ;;
    --fail) fail=1 ;;
    --quiet) quiet=1 ;;
    # stop option processing
    --) shift; break ;;
    # Forward all other options to clippy-driver
    *) driver_options+=( "$opt" ) ;;
  esac
  shift
done
test -z "$prev_out" || { echo "Option is missing argument to set $prev_opt." ; exit 1;}

test -n "$output" || { echo "Error: --output required, but missing." ; exit 1 ;}
test -n "$jq" || { echo "Error: --jq required, but missing." ; exit 1 ;}

# After -- the remaining args are the clippy-driver command and args set
# in the clippy GN template.
filtered_driver_options=()
for opt in "${driver_options[@]}" "$@"
do
  # Extract optarg from --opt=optarg
  case "$opt" in
    -*=*) optarg="${opt#*=}" ;;  # remove-prefix, shortest-match
  esac
  case "$opt" in
  # Same transformation done in 'build/rbe/local-only.sh'
  --local-only=* ) filtered_driver_options+=( "$optarg" ) ;;
  --remote* ) ;;  # pseudoflag for RBE parameters, drop it
  *) filtered_driver_options+=( "$opt" )
  esac
done

# $deps_rspfile contains --externs for direct dependencies
deps_rspfile="$output.deps"
transdeps=( $(sort -u "$output.transdeps") )

# Rewrite --externs to use rmetas where possible.
# Use rmetas where they exist, to avoid requiring local copies of full rlibs.
# Remote-build rlibs do not need to be downloaded, only their .rmeta are needed.
# Assume $deps_rspfile is formatted with one --extern per line.
while read line
do
  case "$line" in
    --extern=*=*.rlib)
      mapping="${line#--extern=}"  # remove prefix (shortest-match)
      lib_name="${mapping%%=*}"  # remove suffix to get lib name (longest-match)
      lib_path="${mapping#*=}"  # remove prefix to get lib path (shortest-match)
      rmeta="${lib_path/.rlib/.rmeta}"
      # If the .rmeta exists, use it.
      if [[ -r "$rmeta" ]]
      then echo "--extern=$lib_name=$rmeta"
      else
        echo "Expecting $rmeta to exist, but did not find it." >&2
        exit 1
      fi
      ;;
    *)  # No change for all other cases, including --extern=...=*.so
      echo "$line"
      ;;
  esac
done < "$deps_rspfile" > "$deps_rspfile.alt"

command=(
  "${filtered_driver_options[@]}"
  -Zno_codegen
  @"$deps_rspfile.alt"
  "${transdeps[@]}"
  --emit metadata="$output.rmeta"
  --error-format=json
  --json=diagnostic-rendered-ansi
)

RUSTC_LOG=error "${command[@]}" 2>"$output"
result="$?"

# clean-up temporary files
[[ "$result" != 0 ]] || {
  rm -f "$deps_rspfile.alt"
}

# Print any detected lints if --quiet wasn't passed
if [[ "$quiet" = 0 ]]; then
  "$jq" -sre '.[] | select((.level == "error") or (.level == "warning")) | .rendered' "$output" || cat "$output" >&2
fi

# Only fail the build with a nonzero exit code if --fail was passed
if [[ "$fail" = 1 ]]; then
  exit "$result"
fi
