#!/bin/bash
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
#
# See usage().

set -e

script="$0"
script_dir="$(dirname "$script")"

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
  All other flags before -- are forwarded to the reproxy bootstrap.
EOF
}

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
    # stop option processing
    --) shift; break ;;
    # Forward all other options to reproxy
    *) bootstrap_options+=("$opt") ;;
  esac
  shift
done
test -z "$prev_out" || { echo "Option is missing argument to set $prev_opt." ; exit 1;}

reproxy_cfg="$config"

bootstrap="$reclient_bindir"/bootstrap
reproxy="$reclient_bindir"/reproxy

# Establish a single log dir per reproxy instance so that statistics are
# accumulated per build invocation.
date="$(date +%Y%m%d-%H%M%S)"
reproxy_logdir="$(mktemp -d -t reproxy."$date".XXXX)"
server_address=unix://"$reproxy_logdir"/reproxy.sock

# deps cache dir should be somewhere persistent between builds,
# and thus, not random.  /var/cache can be root-owned and not always writeable.
if test -n "$HOME"
then _RBE_cache_dir="$HOME/.cache/reproxy/deps"
else _RBE_cache_dir="/tmp/.cache/reproxy/deps"
fi
mkdir -p "$_RBE_cache_dir"

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

# These environment variables take precedence over those found in --cfg.
rewrapper_log_dir="$reproxy_logdir/rewrapper-logs"
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
)

# Check authentication.
auth_option=()
if [[ -n "${USER+x}" ]]
then
  gcert="$(which gcert)"
  if [[ "$?" = 0 ]]
  then
    # In a Corp environment.
    # For developers (not infra), automatically use LOAS credentials
    # to acquire an OAuth2 token.  This saves a step of having to
    # authenticate with a second mechanism.
    # bootstrap --automatic_auth is available in re-client 0.97+
    # See go/rbe/dev/x/reclientoptions#autoauth
    auth_option+=( --automatic_auth=true )
    # bootstrap will call gcert (prompting the user) as needed.
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
if [[ -f "$fx_build_metrics_config" ]]
then source "$fx_build_metrics_config"
fi

test "$BUILD_METRICS_ENABLED" = 0 || {
  if which uuidgen 2>&1 > /dev/null
  then build_uuid="$(uuidgen)"
  else
    cat <<EOF
'uuidgen' is required for logs collection, but missing.
On Debian/Ubuntu platforms, try: 'sudo apt install uuid-runtime'
EOF
    exit 1
  fi
}

# reproxy wants temporary space on the same device where
# the build happens.  The default $TMPDIR is not guaranteed to
# be on the same physical device.
# Re-use the randomly generated dir name in a custom tempdir.
build_dir="${FUCHSIA_BUILD_DIR:-.}"
reproxy_tmpdir="$build_dir"/reproxy_tmpdirs/"$(basename "$reproxy_logdir")"
mkdir -p "$reproxy_tmpdir"

function cleanup() {
  rm -rf "$reproxy_tmpdir"
}

# Startup reproxy.
# Use the same config for bootstrap as for reproxy.
# This also checks for authentication, and prompts the user to
# re-authenticate if needed.
"${bootstrap_env[@]}" \
  "$bootstrap" \
  --re_proxy="$reproxy" \
  --cfg="$reproxy_cfg" \
  "${auth_option[@]}" \
  "${bootstrap_options[@]}"

test "$BUILD_METRICS_ENABLED" = 0 || {
  # Pre-authenticate for uploading metrics and logs
  "$script_dir"/upload_reproxy_logs.sh --auth-only

  # Generate a uuid for uploading logs and metrics.
  echo "$build_uuid" > "$reproxy_logdir"/build_id
}

shutdown() {
  # b/188923283 -- added --cfg to shut down properly
  "${bootstrap_env[@]}" \
    "$bootstrap" \
    --shutdown \
    --cfg="$reproxy_cfg"

  cleanup

  test "$BUILD_METRICS_ENABLED" = 0 || {
    # This script uses the 'bq' CLI tool, which is installed in the
    # same path as `gcloud`.
    # This is experimental and runs a bit noisily for the moment.
    # TODO(https://fxbug.dev/93886): make this run silently
    cloud_project=fuchsia-engprod-metrics-prod
    dataset=metrics
    "$script_dir"/upload_reproxy_logs.sh \
      --reclient-bindir="$reclient_bindir" \
      --uuid="$build_uuid" \
      --bq-logs-table="$cloud_project:$dataset".rbe_client_command_logs_developer \
      --bq-metrics-table="$cloud_project:$dataset".rbe_client_metrics_developer \
      "$reproxy_logdir"
      # The upload exit status does not propagate from inside a trap call.
  }
}

# EXIT also covers INT
trap shutdown EXIT

# original command is in "$@"
# Do not 'exec' this, so that trap takes effect.
"${rewrapper_env[@]}" "$@"
