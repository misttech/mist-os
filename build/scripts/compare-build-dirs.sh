#!/bin/bash
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

function usage() {
  cat <<EOF
usage: $0 DIR1 DIR2 [RELPATH]

Compares two build directories for artifact differences.
Comparison logic tries to account for the file type in
choosing a diff strategy.

If RELPATH is provided, then compare DIR1/RELPATH vs. DIR2/RELPATH.
The joined path may reference a directory or file.

Run this from the Fuchsia source checkout root dir ($FUCHSIA_DIR),
because it references some tools from the source tree.

Example: Compare two clean builds with same output dir:
  fx set ...
  fx clean-build
  cp -p -r out/default out/default.bkp
  fx clean-build
  cp -p -r out/default out/default.bkp2
  $0 out/default.bkp out/default.bkp2

EOF
}

script="$0"  # This is the name of the invoking script, not this one.
script_basename="$(basename "$script")"
script_dir="$(dirname "$script")"

source "$script_dir"/../../build/rbe/common-setup.sh

project_root="$default_project_root"

# json formatting (as pipe): jq . < stdin > stdout
readonly jq="$project_root"/prebuilt/third_party/jq/"$HOST_PLATFORM"/bin/jq

function json_format() {
  "$jq" . < "$1" > "$1".formatted
}

# GLOBAL MUTABLES
# Accumulate paths to unexpected differences here.
# bash: array variables are not POSIX
unexpected_diffs=()
unexpected_matches=()
# These files match or differ, but we didn't know what to expect.
unclassified_diffs=()
unclassified_matches=()
# These files were skipped.
# It might be useful to compare these after ALL
# other differences have been eliminated.
skipped_files=()
# Ignored files are unimportant will never be compared.
ignored_files=()
unspecified_files=()

function diff_json() {
  json_format "$1" || { echo "Failed to format $1" ; return 1;}
  json_format "$2" || { echo "Failed to format $2" ; return 1;}
  diff -u "$1".formatted "$2".formatted
  # return with the exit code of diff
}

function diff_text() {
  diff -u "$1" "$2"
}

function diff_binary() {
  diff -q "$1" "$2"
}

function exe_unstripped_expect() {
  # Print "diff/match/unknown" depending on whether this file is known to be
  # reproducible across identical builds.
  local path="$1"
  # e.g. exe.unstripped/tool_name
  base="$(basename "$path")"
  case "$base" in
    archivist* | \
    base_resolver | \
    cobalt | \
    cobalt_app_unittests | \
    codec_runner_sw_aac | \
    component_manager | \
    component_manager_boot_env | \
    component_manager_test)
    # TODO(fangism): add many more cases of known diffs
          echo "diff" ;;
    *) echo "match" ;;
  esac
}

function diff_file_relpath() {

  # $1 is left dir
  # $2 is right dir
  # $3 is a relative path under both dirs, and is itself not a directory.
  #   This file name is reported in diagnostics.
  # one could also use an all-inclusive diff tool like https://diffoscope.org/
  local left="$1/$3"
  local right="$2/$3"
  local common_path="$3"
  filebase="$(basename "$common_path")"

  # TODO(fangism): Some files are stored as blobs so content differences
  # appear as filename entry differences.  Skip these.  Perhaps silently?
  # Testing -r also allows for symlinks.
  test -r "$left" || {
    printf "%s does not exist\n" "$left"
    return
  }
  test -r "$right" || {
    echo "%s does not exist\n" "$right"
    return
  }

  expect=""

  # Classify each category of files with expectations in each case below:
  #   expect={diff,match,unknown,ignore,skip}; diff...
  # Leave blank for ignored files (not compared).
  # "unknown" means unclassified and could contain a mix of matches/diffs.
  # Goal:
  #   * Identify and classify known differences (eliminate unknowns).
  #   * Gradually reduce sources of differences.
  #
  # TODO(fangism): different expectations in clean-vs-clean /
  # clean-vs-incremental modes.
  case "$filebase" in
    # The exit status of this case statement will be used to determine
    # whether or not the given file is an erroneous diff.
    #
    # Generally:
    #   diff_text for text files that are expected to match
    #   diff_binary for binaries or known large textual differences

    # depfiles
    *.d) expect=match; diff_text "$left" "$right" ;;

    # C++ object files (binary)
    *.o)
      case "$common_path" in
        efi_x64/obj/src/firmware/*.c.o) expect=diff ;;
        efi_x64/obj/src/*.c.o) expect=unknown ;;

        efi_x64/obj/zircon/system/ulib/*.c.o) expect=diff ;;
        efi_x64/obj/zircon/third_party/ulib/cksum/*.c.o) expect=diff ;;
        efi_x64/obj/zircon/*.c.o) expect=unknown ;;

        # See http://fxbug.dev/128466 for aac vs. __TIME__ macros.
        obj/third_party/android/platform/external/aac/*.cpp.o) expect=unknown ;;
        *) expect=match ;;
      esac
      diff_binary "$left" "$right"
      # TODO(fangism): compare objdumps for details
      ;;
    *.so) expect=unknown; diff_binary "$left" "$right" ;;

    # Ignore .a differences until .o differences have been eliminated.
    # Eventually, use diff_binary.
    *.a) expect=skip ;;

    # Rust libraries (binary)
    *.rlib)
      case "$common_path" in
        host_arm64/obj/*.rlib) expect=unknown ;;
        *) expect=match ;;
      esac
      diff_binary "$left" "$right"
      ;;

    # There may be unexpected rmeta mismatches.
    # See http//fxbug.dev/129074, https://github.com/rust-lang/rust/issues/113584
    *.rmeta) expect=match; diff_binary "$left" "$right" ;;

    # Generated code
    *.rs)
      case "$common_path" in
        gen/src/*/qmi-protocol.rs)
          expect=diff ;;  # ordering diff
        *) expect=match ;;
      esac
      diff_text "$left" "$right"
      ;;

    *.vbmeta) expect=unknown; diff_binary "$left" "$right" ;;

    memory_metrics_registry.cb.h)
      expect=diff; diff_text "$left" "$right" ;;  # ordering diff

    # The following groups of files have known huge diffs,
    # so omit details from the general report, and diff_binary.
    meta.far) expect=unknown; diff_binary "$left" "$right" ;;
    contents) expect=unknown; diff_binary "$left" "$right" ;;
    all_blobs.json) expect=skip ;;  # too big right now

    blobs.json) expect=diff; diff_binary "$left" "$right" ;;
      # Many of blobs.json do actually match

    blob.manifest) expect=diff; diff_binary "$left" "$right" ;;  # many hashes
    blobs.manifest) expect=unknown; diff_binary "$left" "$right" ;;
    package_manifest.json) expect=unknown; diff_binary "$left" "$right" ;;
    targets.json)
      case "$common_path" in
        gen/gopaths/*) expect=match; diff_json "$left" "$right"  ;;
        amber-files/repository/targets.json) expect=skip ;; # too big right now
        *.unzipped/*.targets.json) expect=skip ;;  # diffs: sig, expires
        *) expect=diff ;;  # diffs: many hashes
      esac
      ;;

    snapshot.json | *.unzipped/*.snapshot.json) expect=skip ;;  # diffs: sig, expires, version (use diff_json)
    timestamp.json)
      case "$common_path" in
        amber-files/repository/timestamp.json) expect=diff ;; # diffs: sig, expires, version
        *) expect=match ;;
      esac
      diff_json "$left" "$right"
      ;;
    elf_sizes.json) expect=diff; diff_json "$left" "$right" ;;  # diffs: build_id
    recovery-eng_blobs.json) expect=diff; diff_json "$left" "$right" ;;  # diffs: bytes, merkle, size (ordering)
    *.zbi.json) expect=unknown; diff_json "$left" "$right" ;;  # diffs: crc32, size
    update_packages.manifest.json) expect=diff; diff_json "$left" "$right" ;;  # hashes

    images.json) expect=skip ;;  # too many diffs

    compile_commands.json) expect=skip ;;  # too many

    # Diff formatted JSON for readability.
    *.json)
      case "$common_path" in
        host-tools/goroot/src/cmd/interna/test2json/testdata/*.json)
          expect=unknown ;;
        *) expect=match ;;
      esac
      diff_json "$left" "$right"
      ;;
    *.json.formatted) expect=ignore ;;  # This is remant from an earlier diff.

    # recovery things
    recovery-eng_additional_boot_args.txt) expect=diff; diff_text "$left" "$right" ;;  # hashes
    recovery-eng_pkgsvr_index) expect=diff; diff_text "$left" "$right" ;;  # hashes

    update_packages.manifest) expect=diff; diff_text "$left" "$right" ;;  # hashes

    *.image_assembly_inputs) expect=skip ;;  # too many blob/hash differences
    *.image_assembler_all_inputs) expect=skip ;;  # too many blob/hash differences

    # Bazel-related outputs
    java.log)
      case "$common_path" in
        gen/build/bazel/*/java.log) expect=ignore ;;  # log with timestamps
        *) expect=unknown ;;
      esac
      ;;

    lock)
      case "$common_path" in
        gen/build/bazel/*/lock) expect=ignore ;;  # contains PID
        *) expect=unknown ;;
      esac
      ;;

    command_port)
      case "$common_path" in
        gen/build/bazel/*/command_port) expect=ignore ;;  # randomly chosen port
        *) expect=unknown ;;
      esac
      ;;

    server.starttime) expect=ignore ;;  # timestamp
    server.pid.txt) expect=ignore ;;  # contains PID

    command.log)
      case "$common_path" in
        gen/build/bazel/*/command.log) expect=ignore ;;  # subject to build parallelism and completion ordering
        *) expect=unknown ;;
      esac
      ;;

    ffx.log)
      case "$common_path" in
        gen/build/bazel/*/cache/logs/ffx.log) expect=ignore ;;  # log with timestamps
        *) expect=unknown ;;
      esac
      ;;

    volatile-status.txt) expect=diff; diff_text "$left" "$right" ;;  # bears timestamp

    test.cache_status)
      case "$common_path" in
        bazel-out/*/test.cache_status) expect=diff; diff_binary "$left" "$right" ;;
        *) expect=unknown ;;
      esac
      ;;

    test.log)
      case "$common_path" in
        bazel-out/*/test.log) expect=ignore ;;  # reports test time duration
        *) expect=unknown ;;
      esac
      ;;

    test.xml)
      case "$common_path" in
        bazel-out/*/test.xml) expect=ignore ;;  # reports test time duration
        *) expect=unknown ;;
      esac
      ;;

    # Various binaries.
    *.blk) expect=unknown; diff_binary "$left" "$right" ;;
    *.vboot) expect=unknown; diff_binary "$left" "$right" ;;
    *.zbi) expect=unknown; diff_binary "$left" "$right" ;;

    # Most archives carry timestamp information of their contents.
    # One way to make this reproducible is to force a magic date/time
    # while archiving, which effectively removes time variance.
    *.tar | *.tar.gz | *.tgz | *.pyz | *.zip) expect=diff; diff_binary "$left" "$right" ;;

    # Ignore ninja logs, as they bear timestamps,
    # and are non-essential build artifacts.
    .ninja.log) expect=ignore ;;

    # Ignore filesystem access trace files (fsatrace logs).
    # They may contain nondeterministic paths to /proc/PID
    *_trace.txt) expect=ignore ;;

    # like exe.unstripped/*.map files
    # .map files are side-effect outputs of linking binaries, and not consumed
    # anywhere else important.
    # Many of these (but not all) reference mktemp paths (Rust toolchain).
    *.map) expect=ignore ;;

    # Ignore stamp files.
    *.stamp) expect=ignore ;;

    # Ignore temporary and backup files.
    *.tmp) expect=ignore ;;
    *.bak) expect=ignore ;;
    *.bkp) expect=ignore ;;

    # All others.
    # Binary files diffs will still only be reported tersely.
    *)
      file_type="$(file "$left" | head -n 1 | cut -d: -f2-)"
      case "$file_type" in
        # Binary examples:
        # ELF 64-bit LSB pie executable, x86-64, version 1 (SYSV),... dynamically linked, interpreter ld.so.1, no section header
        # Mach-O universal binary with 2 architectures: [x86_64:Mach-O 64-bit executable x86_64] [arm64e:Mach-O 64-bit executable arm64e]
        *ELF*executable* | *Mach-O*binary*)
          case "$common_path" in
            exe.unstripped/*) expect="$(exe_unstripped_expect "$common_path")" ;;
            *) expect=unknown ;;
          esac
          diff_binary "$left" "$right"
          ;;
        # Assume non-binaries are text.
        *) expect=match; diff_text "$left" "$right" ;;
      esac
  esac

  # Record unexpected and unclassified differences/matches.
  diff_status="$?"
  case "$expect" in
    match)
      test "$diff_status" = 0 || unexpected_diffs+=("$common_path") ;;
    diff)
      test "$diff_status" != 0 || unexpected_matches+=("$common_path") ;;
    unknown)
      if test "$diff_status" = 0
      then unclassified_matches+=("$common_path")
      else unclassified_diffs+=("$common_path")
      fi
      ;;
    ignore) ignored_files+=("$common_path") ;;
    skip) skipped_files+=("$common_path") ;;
    *) unspecified_files+=("$common_path") ;;
  esac
}

function diff_select() {
  # $1 and $2 are two directories, e.g. "out/default"
  # $3 is the relative path down from $1 and $2, e.g. "subdir/" or "".
  local relpath="$3"
  local fullpath="$1/$3"
  if test -d "$fullpath"
  then diff_dir_recursive "$1" "$2" "$relpath/"
    # TODO(fangism): what about test -L for symlinks?
  else diff_file_relpath "$1" "$2" "$relpath"
  fi
}

function diff_dir_recursive() {
  # $1 and $2 are two directories, e.g. "out/default"
  # $3 is the relative path down from $1 and $2, e.g. "subdir/" or "".
  # For dual-traversal, arbitrarily use $2's subdirs.
  # echo "Comparing: $sub"
  local sub="$3"  # sub-dir or file

  # Ignore some dirs.
  case "$sub" in
    # Ignore files whose names are content-hash like.
    amber-files/repository/blobs/) return ;;
    amber-files/repository/targets/) return ;;
    *) ;;  # continue
  esac

  # Silence empty dirs.
  if ! ls "$2/$sub"*
  then return  # empty dir
  fi > /dev/null 2>&1

  for f in "$2/$sub"*
  do
    filebase="$(basename "$f")"
    relpath="$sub$filebase"
    diff_select "$1" "$2" "$relpath"
  done
}

test "$#" -ge 2 || { usage; exit 2; }

if test "$#" = 3
then diff_select "$1" "$2" "$3"
else diff_dir_recursive "$1" "$2" ""
fi

# Summarize findings:
echo "======== COMPARISON SUMMARY ========"
exit_status=0

if test "${#unexpected_diffs[@]}" != 0
then
  echo "UNEXPECTED DIFFS: (action: fix source of difference)"
  for path in "${unexpected_diffs[@]}"
  do echo "  $path"
  done
  echo "end of UNEXPECTED DIFFS"
  echo
  exit_status=1
fi

# Good news: these files matched
if test "${#unexpected_matches[@]}" != 0
then
  echo "UNEXPECTED MATCHES: (action: make these expect=match now?)"
  for path in "${unexpected_matches[@]}"
  do echo "  $path"
  done
  echo "end of UNEXPECTED MATCHES"
  echo
  exit_status=1
fi

# This group mismatched, but we didn't know what to expect.
if test "${#unclassified_diffs[@]}" != 0
then
  echo "UNCLASSIFIED DIFFS: (action: classify them as expect=diff)"
  for path in "${unclassified_diffs[@]}"
  do echo "  $path"
  done
  echo "end of UNCLASSIFIED DIFFS"
  echo
  # Leave exit status as it were.
fi

# This group matched, but we didn't know what to expect.
if test "${#unclassified_matches[@]}" != 0
then
  echo "UNCLASSIFIED MATCHES: (action: classify them as expect=match)"
  for path in "${unclassified_matches[@]}"
  do echo "  $path"
  done
  echo "end of UNCLASSIFIED MATCHES"
  echo
  # Leave exit status as it were.
fi

if test "${#skipped_files[@]}" != 0
then
  echo "SKIPPED FILES: (action: compare these after all other differences have been resolved)"
  for path in "${skipped_files[@]}"
  do echo "  $path"
  done
  echo "end of SKIPPED FILES"
  echo
  # Leave exit status as it were.
fi

# Don't bother reporting "${ignored_files[@]}", the list is long and not useful.

# Make sure all cases are covered.
if test "${#unspecified_files[@]}" != 0
then
  echo "UNSPECIFIED FILES: (action: classify into one of the above categories)"
  for path in "${unspecified_files[@]}"
  do echo "  $path"
  done
  echo "end of UNSPECIFIED FILES"
  echo
  exit_status=1
fi

echo "Exiting with status $exit_status"
exit "$exit_status"
