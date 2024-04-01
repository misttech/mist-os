#!/bin/bash
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
#
# See usage().

set -e

readonly script="$0"
# assume script is always with path prefix, e.g. "./$script"
readonly script_dir="${script%/*}"  # dirname

source "$script_dir"/common-setup.sh

readonly PREBUILT_OS="$_FUCHSIA_RBE_CACHE_VAR_host_os"
readonly PREBUILT_ARCH="$_FUCHSIA_RBE_CACHE_VAR_host_arch"

# The project_root must cover all inputs, prebuilt tools, and build outputs.
# This should point to $FUCHSIA_DIR for the Fuchsia project.
# ../../ because this script lives in build/rbe.
# The value is an absolute path.
project_root="$default_project_root"
project_root_rel="$(relpath . "$project_root")"

# defaults
readonly config="$script_dir"/fuchsia-reproxy.cfg

readonly PREBUILT_SUBDIR="$PREBUILT_OS"-"$PREBUILT_ARCH"

readonly build_summary_script="$script_dir"/build_summary.py

# location of reclient binaries relative to output directory where build is run
reclient_bindir="$project_root_rel"/prebuilt/proprietary/third_party/reclient/"$PREBUILT_SUBDIR"

# Configuration for RBE metrics and logs collection.
readonly fx_build_metrics_config="$project_root_rel"/.fx-build-metrics-config

usage() {
  cat <<EOF
$script [reproxy options] -- command [args...]

This script runs reproxy around another command(s).
reproxy is a proxy process between reclient and a remote back-end (RBE).
It needs to be running to host rewrapper commands, which are invoked
by a build system like 'make' or 'ninja'.

example:
  $script -- ninja

options:
  --cfg FILE: reclient config for reproxy
  --bindir DIR: location of reproxy tools
  -t: print additional timestamps for measuring overhead.
  -v | --verbose: print events verbosely
  All other flags before -- are forwarded to the reproxy bootstrap.

environment variables:
  FX_REMOTE_BUILD_METRICS: set to 0 to skip anything related to RBE metrics
    This was easier than plumbing flags through all possible paths
    that call 'fx build'.
EOF
}

verbose=0
print_times=0
bootstrap_options=()
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
    --cfg=*) config="$optarg" ;;
    --cfg) prev_opt=config ;;
    --bindir=*) reclient_bindir="$optarg" ;;
    --bindir) prev_opt=reclient_bindir ;;
    -t) print_times=1 ;;
    -v | --verbose) verbose=1 ;;
    # stop option processing
    --) shift; break ;;
    # Forward all other options to reproxy
    *) bootstrap_options+=("$opt") ;;
  esac
  shift
done
test -z "$prev_out" || { echo "Option is missing argument to set $prev_opt." ; exit 1;}

function _timetrace() {
  [[ "$print_times" == 0 ]] || timetrace "$@"
}

_timetrace "main start (after option processing)"

readonly reproxy_cfg="$config"
readonly bootstrap="$reclient_bindir"/bootstrap
readonly reproxy="$reclient_bindir"/reproxy

# Establish a single log dir per reproxy instance so that statistics are
# accumulated per build invocation.
readonly date="$(date +%Y%m%d-%H%M%S)"
readonly build_dir_file="$project_root_rel/.fx-build-dir"
build_subdir=out/unknown
if test -f "$build_dir_file"
then
  # Locate the reproxy logs and temporary dirs on the same device as
  # the build output, so that moves can be done atomically,
  # and this avoids cross-device linking problems.
  build_dir_file_contents="$(cat "$build_dir_file")"
  # In some cases, .fx-build-dir might contain an absolute path.
  # We want only the relative-path.
  if [[ "$build_dir_file_contents" =~ ^/ ]]
  then build_subdir="$(relpath "$project_root" "$build_dir_file_contents")"
  else build_subdir="$build_dir_file_contents"
  fi
fi
# assume build_subdir path has depth=2
IFS=/ read -r -a build_subdir_arr <<< "$build_subdir"

[[ "${#build_subdir_arr[@]}" == 2 ]] || {
  cat <<EOF
Warning: expected a relative build subdir with 2 components, but got ${build_subdir_arr[@]}.
If you see this, file a go/fx-build-bug.
EOF
}

readonly old_logs_root="$project_root/$build_subdir/.reproxy_logs"
# Move the reproxy logs outside of $build_subdir so they do not get cleaned,
# but under 'out' so it does not pollute the source root.
readonly logs_root="$project_root/${build_subdir_arr[0]}/.reproxy_logs/${build_subdir_arr[1]}"
mkdir -p "$old_logs_root"
mkdir -p "$logs_root"

# 'mktemp -p' still yields to TMPDIR in the environment (bug?),
# so override TMPDIR instead.
readonly reproxy_logdir="$(env TMPDIR="$logs_root" mktemp -d -t "reproxy.$date.XXXX")"
readonly log_base="${reproxy_logdir##*/}"  # basename

readonly _fake_tmpdir="$(mktemp -u)"
readonly _tmpdir="${_fake_tmpdir%/*}"  # dirname
# Symlink to the old locations, where users may be accustomed to looking.
ln -s -f "$reproxy_logdir" "$_tmpdir"/
( cd "$old_logs_root" && ln -s "$reproxy_logdir" . )

# The socket file doesn't need to be co-located with logs.
# Using an absolute path to the socket allows rewrapper to be invoked
# from different working directories.
readonly socket_path="$_tmpdir/$log_base.sock"
test "${#socket_path}" -le 100 || {
  cat <<EOF
Socket paths are limited to around 100 characters on some platforms.
  Got: $socket_path (${#socket_path} characters)
See https://unix.stackexchange.com/questions/367008/why-is-socket-path-length-limited-to-a-hundred-chars
EOF
  exit 1
}
readonly server_address="unix://$socket_path"

# deps cache dir should be somewhere persistent between builds,
# and thus, not random.  /var/cache can be root-owned and not always writeable.
if test -n "$HOME"
then _RBE_cache_dir="$HOME/.cache/reproxy/deps"
else _RBE_cache_dir="/tmp/.cache/reproxy/deps"
fi
mkdir -p "$_RBE_cache_dir"

[[ "${USER-NOT_SET}" != "NOT_SET" ]] || {
  echo "Error: USER must be set to authenticate using RBE."
  exit 1
}

# These environment variables take precedence over those found in --cfg.
bootstrap_env=(
  env
  TMPDIR="$reproxy_tmpdir"
  RBE_proxy_log_dir="$reproxy_logdir"
  RBE_log_dir="$reproxy_logdir"
  RBE_server_address="$server_address"
  # rbe_metrics.{pb,txt} appears in -output_dir
  RBE_output_dir="$reproxy_logdir"
  RBE_cache_dir="$_RBE_cache_dir"
)

if [[ "${FX_BUILD_UUID-NOT_SET}" == "NOT_SET" ]]
then
  build_uuid=$("$python" -S -c 'import uuid; print(uuid.uuid4())')
else
  build_uuid="${FX_BUILD_UUID}"
fi

# These environment variables take precedence over those found in --cfg.
readonly rewrapper_log_dir="$reproxy_logdir/rewrapper-logs"
mkdir -p "$rewrapper_log_dir"
rewrapper_env=(
  env
  RBE_server_address="$server_address"
  # rewrapper logs
  RBE_log_dir="$rewrapper_log_dir"
  # Technically, RBE_proxy_log_dir is not needed for rewrapper,
  # however, the reproxy logs do contain information about individual
  # rewrapper executions that could be discovered and used in diagnostics,
  # so we include it.
  RBE_proxy_log_dir="$reproxy_logdir"
  # Identify which build each remote action request comes from.
  RBE_invocation_id="user:$build_uuid"
)

# Check authentication.
auth_option=()
if [[ -n "${USER+x}" ]]
then
  gcert="$(which gcert)"
  if [[ "$?" = 0 ]]
  then
    # In a Corp environment.
    # Use the credentials helper named in the cfg.
    :
  else
    # Everyone else uses gcloud authentication.
    gcloud="$(which gcloud)" || {
      cat <<EOF
\`gcloud\` command not found (but is needed to authenticate).
\`gcloud\` can be installed from the Cloud SDK:

  http://go/cloud-sdk#installing-and-using-the-cloud-sdk

EOF
      exit 1
    }

    # Instruct user to authenticate if needed.
    "$gcloud" auth list 2>&1 | grep -q "$USER@google.com" || {
      cat <<EOF
Did not find credentialed account (\`gcloud auth list\`): $USER@google.com.
You may need to re-authenticate every 20 hours.

To authenticate, run:

  gcloud auth login --update-adc

EOF
      exit 1
    }
  fi
fi


# If configured, collect reproxy logs.
BUILD_METRICS_ENABLED=0
if [[ "$FX_REMOTE_BUILD_METRICS" == 0 ]]
then echo "Disabled RBE metrics for this run."
else
  if [[ -f "$fx_build_metrics_config" ]]
  then source "$fx_build_metrics_config"
    # This config sets BUILD_METRICS_ENABLED.
  fi
fi

# reproxy wants temporary space on the same device where
# the build happens.  The default $TMPDIR is not guaranteed to
# be on the same physical device.
# Re-use the randomly generated dir name in a custom tempdir.
reproxy_tmpdir="$project_root/$build_subdir"/.reproxy_tmpdirs/"$log_base"
mkdir -p "$reproxy_tmpdir"

function cleanup() {
  rm -rf "$reproxy_tmpdir"
}

# Startup reproxy.
# Use the same config for bootstrap as for reproxy.
# This also checks for authentication, and prompts the user to
# re-authenticate if needed.
_timetrace "Bootstrapping reproxy"
"${bootstrap_env[@]}" \
  "$bootstrap" \
  --re_proxy="$reproxy" \
  --cfg="$reproxy_cfg" \
  "${bootstrap_options[@]}" > "$reproxy_logdir"/bootstrap.stdout
[[ "$verbose" != 1 ]] || {
  cat "$reproxy_logdir"/bootstrap.stdout
  echo "build id: $build_uuid"
  echo "logs: $reproxy_logdir"
  echo "socket: $socket_path"
}
_timetrace "Bootstrapping reproxy (done)"

test "$BUILD_METRICS_ENABLED" = 0 || {
  _timetrace "Authenticating for metrics upload"
  # Pre-authenticate for uploading metrics and logs
  "$script_dir"/upload_reproxy_logs.sh --auth-only

  # Generate a uuid for uploading logs and metrics.
  echo "$build_uuid" > "$reproxy_logdir"/build_id
  _timetrace "Authenticating for metrics upload (done)"
}

shutdown() {
  _timetrace "Shutting down reproxy"
  # b/188923283 -- added --cfg to shut down properly
  "${bootstrap_env[@]}" \
    "$bootstrap" \
    --shutdown \
    --cfg="$reproxy_cfg" > "$reproxy_logdir"/shutdown.stdout
  [[ "$verbose" != 1 ]] || {
    cat "$reproxy_logdir"/shutdown.stdout
  }
  _timetrace "Shutting down reproxy (done)"

  cleanup

  [[ "${FX_BUILD_RBE_STATS-NOT_SET}" == "NOT_SET" ]] || {
    "$python" -S "$build_summary_script" "$reproxy_logdir"
  }

  test "$BUILD_METRICS_ENABLED" = 0 || {
    _timetrace "Processing RBE logs and uploading to BigQuery"
    # This script uses the 'bq' CLI tool, which is installed in the
    # same path as `gcloud`.
    # This is experimental and runs a bit noisily for the moment.
    # TODO(https://fxbug.dev/42175720): make this run silently
    cloud_project=fuchsia-engprod-metrics-prod
    dataset=metrics
    "$script_dir"/upload_reproxy_logs.sh \
      --reclient-bindir="$reclient_bindir" \
      --uuid="$build_uuid" \
      --bq-logs-table="$cloud_project:$dataset".rbe_client_command_logs_developer_raw \
      --bq-metrics-table="$cloud_project:$dataset".rbe_client_metrics_developer_raw \
      "$reproxy_logdir"
      # The upload exit status does not propagate from inside a trap call.
    _timetrace "Processing RBE logs and uploading to BigQuery (done)"
  }
}

# EXIT also covers INT
trap shutdown EXIT

# original command is in "$@"
# Do not 'exec' this, so that trap takes effect.
_timetrace "Running wrapped command"
"${rewrapper_env[@]}" "$@"
_timetrace "Running wrapped command (done)"
