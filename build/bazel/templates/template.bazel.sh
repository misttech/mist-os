#!/bin/bash
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# AUTO-GENERATED - DO NOT EDIT!

readonly _SCRIPT_DIR="$(cd "$(dirname "${{BASH_SOURCE[0]}}")" >/dev/null 2>&1 && pwd)"
readonly _WORKSPACE_DIR="${{_SCRIPT_DIR}}/{workspace}"
readonly _OUTPUT_BASE="${{_SCRIPT_DIR}}/{output_base}"
readonly _OUTPUT_USER_ROOT="${{_SCRIPT_DIR}}/{output_user_root}"
readonly _LOG_DIR="${{_SCRIPT_DIR}}/{logs_dir}"
readonly _BAZEL_BIN="${{_SCRIPT_DIR}}/{bazel_bin_path}"
readonly _PYTHON_PREBUILT_DIR="${{_SCRIPT_DIR}}/{python_prebuilt_dir}"
readonly _REMOTE_SERVICES_BAZELRC="${{_SCRIPT_DIR}}/{ninja_output_dir}/regenerator_outputs/remote_services.bazelrc"

# Exported explicitly to be used by repository rules to reference the
# Ninja output directory and binary.
export BAZEL_FUCHSIA_NINJA_OUTPUT_DIR="${{_SCRIPT_DIR}}/{ninja_output_dir}"
export BAZEL_FUCHSIA_NINJA_PREBUILT="${{_SCRIPT_DIR}}/{ninja_prebuilt}"

# Ensure our prebuilt Python3 executable is in the PATH to run repository
# rules that invoke Python programs correctly in containers or jails that
# do not expose the system-installed one.
export PATH="${{_PYTHON_PREBUILT_DIR}}/bin:${{PATH}}"

# An undocumented, but widely used, environment variable that tells Bazel to
# not auto-detect the host C++ installation. This makes workspace setup faster
# and ensures this can be used on containers where GCC or Clang are not
# installed (Bazel would complain otherwise with an error).
export BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN=1

# Implement log rotation (up to 3 old files)
# $1: log file name (e.g. "path/to/workspace-events.log")
logrotate3 () {{
  local i
  local prev_log="$1.3"
  local cur_log
  for i in "2" "1"; do
    rm -f "${{prev_log}}"
    cur_log="$1.$i"
    if [[ -f "${{cur_log}}" ]]; then
      mv "${{cur_log}}" "${{prev_log}}"
    fi
    prev_log="${{cur_log}}"
  done
  cur_log="$1"
  if [[ -f "${{cur_log}}" ]]; then
    mv "${{cur_log}}" "${{prev_log}}"
  fi
}}

# Rotate the workspace events log. Note that this file is created
# through an option set in the .bazelrc file, not the command-line below.
mkdir -p "${{_LOG_DIR}}"
logrotate3 "${{_LOG_DIR}}/workspace-events.log"

# For infra builds, connections to various remote services are tunneled
# through local socket relays, launched by [infra/infra]/cmd/buildproxywrap/main.go.
# Detect manually provided config options that involve the proxies.
# TODO: This method doesn't work if the same configs are indirectly enabled.
has_remote_config=
siblings_link_template=
proxy_overrides=()
for arg in "$@"
do
  # Check for infra and non-infra config variations to allow for local testing.
  case "$arg" in
    --config=sponge | --config=sponge_infra) # Sponge build event service
      [[ "${{BAZEL_sponge_socket_path-NOT_SET}}" == "NOT_SET" ]] ||
        proxy_overrides+=( "--bes_proxy=unix://$BAZEL_sponge_socket_path" )
        siblings_link_template="http://sponge/invocations/"
      ;;
    --config=resultstore | --config=resultstore_infra) # Resultstore build event service
      [[ "${{BAZEL_resultstore_socket_path-NOT_SET}}" == "NOT_SET" ]] ||
        proxy_overrides+=( "--bes_proxy=unix://$BAZEL_resultstore_socket_path" )
        # Note: go/fxbtx uses project=rbe-fuchsia-prod
        siblings_link_template="http://go/fxbtx/"
      ;;
    --config=remote | --config=remote_cache_only)  # Remote build execution service
      has_remote_config=true
      [[ "${{BAZEL_rbe_socket_path-NOT_SET}}" == "NOT_SET" ]] ||
        proxy_overrides+=( "--remote_proxy=unix://$BAZEL_rbe_socket_path" )
      ;;
  esac
done

# Propagate some build metadata from the environment.
# Some of these values are set by infra.
build_metadata_opts=()
[[ "${{BUILDBUCKET_ID-NOT_SET}}" == "NOT_SET" ]] || {{
  build_metadata_opts+=(
    "--build_metadata=BUILDBUCKET_ID=$BUILDBUCKET_ID"
    "--build_metadata=SIBLING_BUILDS_LINK=${{siblings_link_template}}?q=BUILDBUCKET_ID:$BUILDBUCKET_ID"
  )
  case "$BUILDBUCKET_ID" in
    */led/*)
      build_metadata_opts+=( "--build_metadata=PARENT_BUILD_LINK=http://go/lucibuild/$BUILDBUCKET_ID/+/build.proto" ) ;;
    *)
      build_metadata_opts+=( "--build_metadata=PARENT_BUILD_LINK=http://go/bbid/$BUILDBUCKET_ID" ) ;;
  esac
}}

[[ "${{BUILDBUCKET_BUILDER-NOT_SET}}" == "NOT_SET" ]] ||
  build_metadata_opts+=( "--build_metadata=BUILDBUCKET_BUILDER=$BUILDBUCKET_BUILDER" )

# Developers' builds will have one uuid per `fx build` invocation
# that can be used to correlate multiple bazel sub-builds.
[[ "${{FX_BUILD_UUID-NOT_SET}}" == "NOT_SET" ]] ||
  build_metadata_opts+=(
    "--build_metadata=FX_BUILD_UUID=$FX_BUILD_UUID"
    "--build_metadata=SIBLING_BUILDS_LINK=${{siblings_link_template}}?q=FX_BUILD_UUID:$FX_BUILD_UUID"
  )
  # search for siblings

# Use a shared disk cache if FUCHSIA_BAZEL_DISK_CACHE is set and
# --config=remote is not used. Bazel documentation states that --disk_cache
# is compatible with remote caching, but RBE documentation says otherwise
# so err on the side of caution. This path must be absolute.
#
# This is useful when several checkouts are used on the same machine,
# or when performing repeated clean builds are performed frequently. Note that
# Bazel itself never cleans up the disk cache, as this is left to the user.
[[ -n "${{FUCHSIA_BAZEL_DISK_CACHE}}" && -z "${{has_remote_config}}" ]] &&
  build_metadata_opts+=(--disk_cache="${{FUCHSIA_BAZEL_DISK_CACHE}}")

# In Corp environments with valid gcert credentials, use the credential helper
# to automatically exchange LOAS for OAuth (Google Cloud Platform) tokens.
# This requires less interaction from the user than 'gcloud auth ...'.
use_gcert_auth=()
if [[ "$FX_BUILD_LOAS_TYPE" == "unrestricted" ]]
then use_gcert_auth=(--config=gcertauth)
fi

# Setting $USER so `bazel` won't fail in environments with fake UIDs. Even if
# the USER is not actually used. See https://fxbug.dev/42063551#c9.
# In developer environments, use the real username so that authentication
# and credential helpers will work, e.g. go/bazel-google-sso.
#
# Explanation for flags:
#  --nohome_rc: Ignore $HOME/.bazelrc to enforce hermiticity / reproducibility.
#  --output_base: Ensure the output base is in the Ninja output directory, not under $HOME.
#  --output_user_root: Ensure the output user root is in the Ninja output directory, not under $HOME.
_user="${{USER:-unused-bazel-build-user}}"
cd "${{_WORKSPACE_DIR}}" && USER="$_user" "${{_BAZEL_BIN}}"\
      --nohome_rc \
      --bazelrc="${{_REMOTE_SERVICES_BAZELRC}}" \
      --output_base="${{_OUTPUT_BASE}}" \
      --output_user_root="${{_OUTPUT_USER_ROOT}}" \
      "$@" \
      "${{use_gcert_auth[@]}}" \
      "${{build_metadata_opts[@]}}" \
      "${{proxy_overrides[@]}}"
