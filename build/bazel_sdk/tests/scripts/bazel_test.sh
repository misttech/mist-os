#!/bin/bash

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -e

get_abspath () {
  if [[ ! -e "$1" ]]; then
    echo >&2 "ERROR: get_abspath() requires an existing file/directory path: $1"
    exit 1
  fi
  printf %s/%s "$(cd "$(dirname "$1")" && pwd)" "$(basename "$1")"
}

die () {
  echo >&2 "ERROR: $@"
  exit 1
}

check_for_fx () {
  if ! fx help >/dev/null 2>&1; then
    die "This script's auto-detection logic requires 'fx' to be in your PATH"
  fi
}

main() {
  local stamp_file
  local bazel
  local clean
  local help
  local fuchsia_build_dir
  local fuchsia_source_dir
  local target_cpu
  local verbose
  local output_base
  local output_user_root
  local args=()

  while [[ -n "$1" ]]; do
    OPT="$1"
    shift

    OPTARG=
    case "$OPT" in
      -*=*) OPTARG="${OPT#*=}" ;; # remove-prefix, shorted-match
    esac

    case "$OPT" in
      --help)
         help=true
         ;;
      --bazel=*)
         bazel="${OPTARG}"
         ;;
      --fuchsia_build_dir=*)
         fuchsia_build_dir="${OPTARG}"
         ;;
      --fuchsia_source_dir=*)
         fuchsia_source_dir="${OPTARG}"
         ;;
      --output_base=*)
         output_base="${OPTARG}"
         ;;
      --output_user_root=*)
         output_user_root="${OPTARG}"
         ;;
      --stamp-file=*)
         stamp_file="${OPTARG}"
         ;;
      --target_cpu=*)
         target_cpu="${OPTARG}"
         ;;
      --verbose)
          verbose=true
          ;;
      --clean)
          clean=true
          ;;
      --)
         # Extra args passed directly to bazel test
         args+=("$@")
         break
         ;;
      -*)
         echo >&2 "Unknown option: $OPT"
         return 1
         ;;
      *)
         echo >&2 "This script does not take direct arguments. Use -- to pass arguments to `bazel test`"
         return 1
         ;;
    esac
  done

  if [[ -n "$help" ]]; then
    cat <<EOF
Usage: $(basename $0) [options] [-- <extra bazel test args>]

Run the Fuchsia Bazel SDK test suite in-tree.
You must have built the 'generate_fuchsia_sdk_repository'
target with 'fx build' before calling this script, as in:

  fx build generate_fuchsia_sdk_repository

Valid options:

  --help
     Print this help

  --verbose
     Print information before launching 'bazel test'.

  --clean
     Perform a clean build by expunging the output base first.

  --fuchsia_build_dir=DIR
     Fuchsia build directory (auto-detected).

  --fuchsia_source_dir=DIR
     Fuchsia source directory (auto-detected).

  --output_base=DIR  (optional)
     Specify output base directory, instead of the default one in \$HOME.

  --output_user_root=DIR (optional)
     Specify output user root directory, instead of the default one in \$HOME.

  --target_cpu=CPU
     Fuchsia CPU name (auto-detected).

  --bazel=PROGRAM
     Path to Bazel program to use (default is 'bazel').

  --stamp-file=FILE
     Specify output stamp file, which will be touched on success.

EOF
    return 0
  fi

  if [[ -z "${fuchsia_build_dir}" ]]; then
    check_for_fx
    fuchsia_build_dir="$(fx get-build-dir)"
  fi

  # The Bazel workspace assumes that the Fuchsia cpu is the host
  # CPU unless --cpu or --platforms is used. Extract the target_cpu
  # from ${fuchsia_build_dir}/args.json and construct the corresponding
  # bazel test argument.
  #
  # Note that there is a subtle issue here: for historical reasons, the default
  # --cpu value is `k8`, an old moniker for the x86_64 cpu architecture. This
  # impacts the location of build artifacts in the default/target build
  # configuration, which would go under bazel-out/k8-fastbuild/bin/...
  # then.
  #
  # However, our --config=fuchsia_x64 argument below changes --cpu to `x86_64`
  # which is also recognized properly, but changes the location of build
  # artifacts to bazel-out/x86_64-fastbuild/ instead, which is important when
  # these paths go into build artifacts (e.g. product bundle manifests) and
  # need to be compared to golden files by the test suite.
  #
  # In short, this test suite does not support invoking `bazel test` without
  # an appropriate `--config=fuchsia_<cpu>` argument.
  #

  if [[ -z "${target_cpu}" ]]; then
    check_for_fx
    target_cpu=$(fx jq --raw-output .target_cpu "${fuchsia_build_dir}"/args.json)
  fi

  fuchsia_build_dir="$(get_abspath "${fuchsia_build_dir}")"

  local script_dir="$(dirname "${BASH_SOURCE[0]}")"

  if [[ -z "${fuchsia_source_dir}" ]]; then
    fuchsia_source_dir="$(get_abspath "${script_dir}")"
    while [[ ! -f "${fuchsia_source_dir}/.jiri_manifest" ]]; do
      fuchsia_source_dir="$(dirname "${fuchsia_source_dir}")"
      if [[ "${fuchsia_source_dir}" == "/" ]]; then
        die "Could not find Fuchsia source directory, please use --fuchsia_source_dir=DIR"
      fi
    done
  else
    # fuchsia_source_dir must be an absolute path or Bazel will complain
    # when it is used for --override_repository options below.
    fuchsia_source_dir=$(get_abspath "${fuchsia_source_dir}")
  fi

  # Compute Fuchsia host tag (e.g. linux-x64 or mac-arm64).
  local detected_os="$(uname -s)"
  local detected_arch="$(uname -m)"
  case "${detected_os}" in
    Linux) host_tag="linux";;
    Darwin) ost_tag="mac";;
    *) die "Unsupported host operating system: ${detected_os}";;
  esac
  case "${detected_arch}" in
    x86_64) host_tag="${host_tag}-x64";;
    arm64|aarch64) host_tag="${host_tag}-arm64";;
    *) die "Unsupported host cpu architecture: ${detected_arch}";;
  esac

  if [[ -n "${bazel}" ]]; then
    bazel="$(get_abspath "${bazel}")"
  else
    bazel="${fuchsia_source_dir}/prebuilt/third_party/bazel/${host_tag}/bazel"
  fi

  # Location of the prebuilt python toolchain.
  local python_prebuilt_dir="${fuchsia_source_dir}/prebuilt/third_party/python3/${host_tag}"

  # Assume this script is under `$WORKSPACE/scripts`.
  local workspace_dir="$(cd "${script_dir}/.." && pwd)"

  local downloader_config_file="${workspace_dir}/scripts/downloader_config"

  # These options must appear before the Bazel command.
  local bazel_startup_args=(
    # Disable parsing of $HOME/.bazelrc to avoid unwatned side-effects.
    --nohome_rc
  )
  if [[ -n "${output_user_root}" ]]; then
    mkdir -p "${output_user_root}" || exit 1
    output_user_root="$(get_abspath "${output_user_root}")"
    bazel_startup_args+=(--output_user_root="${output_user_root}")
  fi

  if [[ -n "${output_base}" ]]; then
    mkdir -p "${output_base}" || exit 1
    output_base="$(get_abspath "${output_base}")"
    bazel_startup_args+=(--output_base="${output_base}")
  fi

  # These options must appear after the Bazel command.
  local bazel_common_args=(
    # Ensure binaries are generated for the right Fuchsia CPU architecture.
    # Without this, @fuchsia_sdk rules assume that target_cpu == host_cpu,
    # and will use an incorrect output path prefix (i.e.
    # bazel-out/k8-fastbuild/ instead of bazel-out/x86_64-fastbuild/ on
    # x64 hosts, leading to golden file comparison failures later.
    --config=fuchsia_${target_cpu}

    # Prevent all downloads through a downloader configuration file.
    # Note that --experimental_repository_disable_download does not
    # seem to work at all.
    #
    # Fun fact: the path must be relative to the workspace, or an absolute
    # path, and if the file does not exist, the Bazel server will crash
    # *silently* with a Java exception, leaving no traces on the client
    # terminal :-(
    --experimental_downloader_config="${downloader_config_file}"

    # Ensure the embedded JDK that comes with Bazel is always used
    # This prevents Bazel from downloading extra host JDKs from the
    # network, even when a project never uses Java-related  rules
    # (for some still-very-mysterious reasons!)
    --java_runtime_version=embedded_jdk
    --tool_java_runtime_version=embedded_jdk

    # Ensure outputs are writable (umask 0755) instead of readonly (0555),
    # which prevent removing output directories with `rm -rf`.
    # See https://fxbug.dev/121003
    --experimental_writable_outputs

    # Override repositories since all downloads are forbidden.
    # This allows the original WORKSPACE.bazel to work out-of-tree by
    # download repositories as usual, while running the test suite in-tree
    # will use the Fuchsia checkout's versions of these external dependencies.
    --override_repository=bazel_skylib=${fuchsia_source_dir}/third_party/bazel_skylib
    --override_repository=rules_cc=${fuchsia_source_dir}/third_party/bazel_rules_cc
    --override_repository=rules_python=${fuchsia_source_dir}/third_party/bazel_rules_python
    --override_repository=rules_license=${fuchsia_source_dir}/third_party/bazel_rules_license
    --override_repository=platforms=${fuchsia_source_dir}/third_party/bazel_platforms
    --override_repository=rules_java=${fuchsia_source_dir}/build/bazel/local_repositories/rules_java
  )

  if [[ -n "${clean}" ]]; then
    (cd "${workspace_dir}" && "${bazel}" "${bazel_startup_args[@]}" clean --expunge)
  fi

  bazel_test_command=(
    # Setting USER is required to run Bazel, so force it to run on infra bots.
    USER="${USER:-unused-bazel-build-user}"

    # Pass the location of the Fuchsia build directory to the
    # @fuchsia_sdk repository rule. Note that using --action_env will
    # not work because this option only affects Bazel actions, and
    # not repository rules.
    LOCAL_FUCHSIA_PLATFORM_BUILD="${fuchsia_build_dir}"

    # An undocumented, but widely used, environment variable that tells Bazel to
    # not auto-detect the host C++ installation. This makes workspace setup faster
    # and ensures this can be used on containers where GCC or Clang are not
    # installed (Bazel would complain otherwise with an error).
    BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN=1

    # Ensure our prebuilt Python3 executable is in the PATH to run repository
    # rules that invoke Python programs correctly in containers or jails that
    # do not expose the system-installed one.
    PATH="${python_prebuilt_dir}/bin:${PATH}"

    "${bazel}"

    "${bazel_startup_args[@]}"

    test

    "${bazel_common_args[@]}"

    # By default, bazel test failures will indicate which targets failed
    # but not why. This option ensures that test errors are properly
    # printed to stderr.
    --test_output=errors

    :tests "${args[@]}"
  )

  if [[ -n "${verbose}" ]]; then
    echo
    echo "Using bazel: ${bazel}"
    echo "Using target_cpu: ${target_cpu}"
    echo "Using Fuchsia dir: ${fuchsia_source_dir}"
    echo "Using workspace: ${workspace_dir}"
    echo "Command:"
    echo "("
    echo "  cd ${workspace_dir} &&"
    for CMD in "${bazel_test_command[@]}"; do
       echo "  ${CMD} \\"
    done
    echo ")"
  fi

  (cd "${workspace_dir}" && /usr/bin/env "${bazel_test_command[@]}")

  if [[ -n "${stamp_file}" ]]; then
     touch "$stamp_file"
  fi
}

main "$@"
