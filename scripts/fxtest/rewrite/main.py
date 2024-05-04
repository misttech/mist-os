# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from __future__ import annotations

import argparse
import asyncio
import atexit
from collections import defaultdict
from dataclasses import dataclass
from dataclasses import field
import functools
import gzip
import json
import os
import re
import subprocess
import sys
import tempfile
import textwrap
import typing

import args
import config
import console
import dataparse
import debugger
import environment
import event
import execution
import log
import selection
import selection_types
import statusinfo
import termout
import test_list_file
import tests_json_file
import util.command as command
import util.signals


def main() -> None:
    # Main entrypoint.
    # Set up the event loop to catch termination signals (i.e. Ctrl+C), and
    # cancel the main task when they are received.
    try:
        config_file = config.load_config()
    except argparse.ArgumentError as e:
        print(f"Failed to parse config: {e.message}")
        sys.exit(1)
    try:
        real_flags = args.parse_args(defaults=config_file.default_flags)
    except argparse.ArgumentError as e:
        print(f"Failed to parse command line: {e.message}")
        sys.exit(1)

    # Special utility mode handling
    if real_flags.print_logs:
        assert_no_selection(real_flags, "-pr log")
        warning_message = "WARNING: --print-logs is deprecated and will be removed. Use `--previous log`"
        print(warning_message, "\n")
        ret = do_print_logs(real_flags)
        print(warning_message)
        sys.exit(ret)
    if real_flags.previous is not None:
        sys.exit(do_process_previous(real_flags))

    # No special modes, proceed with async execution.
    fut = asyncio.ensure_future(
        async_main_wrapper(real_flags, config_file=config_file)
    )
    util.signals.register_on_terminate_signal(fut.cancel)  # type: ignore[arg-type]
    try:
        loop = asyncio.get_event_loop()
        loop.run_until_complete(fut)
        sys.exit(fut.result())
    except asyncio.CancelledError:
        print("\n\nReceived interrupt, exiting")
        sys.exit(1)


async def async_main_wrapper(
    flags: args.Flags,
    recorder: event.EventRecorder | None = None,
    config_file: config.ConfigFile | None = None,
) -> int:
    """Wrapper for the main logic of fx test.

    This wrapper creates a list containing tasks that must be
    awaited before the program exits. The main logic may add tasks to this
    list during execution, and then return the intended status code.

    Args:
        flags (args.Flags): Flags to pass into the main function.
        recorder (event.EventRecorder | None, optional): If set,
            use this event recorder. Used for testing.
        config_file (config.ConfigFile, optional): If set, record
            that this configuration was loaded to set default flags.

    Returns:
        The return code of the program.
    """
    tasks: list[asyncio.Task[None]] = []
    if recorder is None:
        recorder = event.EventRecorder()

    ret = await async_main(flags, tasks, recorder, config_file)

    to_wait = asyncio.Task(asyncio.wait(tasks), name="Drain tasks")
    timeout_seconds = 5
    try:
        await asyncio.wait_for(asyncio.shield(to_wait), timeout=timeout_seconds)
    except asyncio.TimeoutError:
        print(
            f"\n\nWaiting for tasks to complete for longer than {timeout_seconds} seconds...\n",
            file=sys.stderr,
        )
        await to_wait

    return ret


def assert_no_selection(flags: args.Flags, suggested_args: str) -> None:
    """Assert that flags do not have any selections, and print an
       error message if they do.

    Args:
        flags (args.Flags): Command flags
        suggested_args (str): Suggestion for what the user should
            run after executing their tests.
    """
    if flags.selection:
        selection_args = " ".join(flags.selection)
        print(
            f"ERROR: --previous mode does not support running tests, it only displays information from your previous run.\nTry running `fx test {selection_args}` and then `fx test {suggested_args}`"
        )
        sys.exit(1)


def do_process_previous(flags: args.Flags) -> int:
    assert_no_selection(flags, f"-pr {flags.previous}")
    if flags.previous is args.PrevOption.LOG:
        return do_print_logs(flags)
    elif flags.previous is args.PrevOption.PATH:
        env = environment.ExecutionEnvironment.initialize_from_args(
            flags, create_log_file=False
        )
        print(env.get_most_recent_log())
        return 0
    elif flags.previous is args.PrevOption.HELP:
        print("--previous options:")
        for arg in args.PrevOption:
            prefix = f"{arg:>8}: "
            print(
                "\n".join(
                    textwrap.wrap(
                        prefix + arg.help(),
                        width=70,
                        initial_indent="  ",
                        subsequent_indent="  " + " " * len(prefix),
                    )
                )
                + "\n"
            )
        return 0
    else:
        print(f"Unknown --previous option {flags.previous}")
        return 1


def do_print_logs(flags: args.Flags) -> int:
    env = environment.ExecutionEnvironment.initialize_from_args(
        flags, create_log_file=False
    )
    try:
        log_path = env.get_most_recent_log()
        with gzip.open(log_path, "rt") as f:
            print(f"{log_path}:\n")
            log.pretty_print(f)
    except environment.EnvironmentError as e:
        print(f"Failed to read log: {e}", file=sys.stderr)
        return 1
    except gzip.BadGzipFile as e:
        print(f"File does not appear to be a gzip file. ({e})", file=sys.stderr)
        return 1

    return 0


async def async_main(
    flags: args.Flags,
    tasks: list[asyncio.Task[None]],
    recorder: event.EventRecorder,
    config_file: config.ConfigFile | None = None,
) -> int:
    """Main logic of fx test.

    Args:
        flags (args.Flags): Flags controlling the behavior of fx test.
        tasks (List[asyncio.Tasks]): List to add tasks to that must be awaited before termination.
        recorder (event.Recorder): The recorder for events.
        config_file (config.ConfigFile, optional): The loaded config, if one was set.

    Returns:
        The return code of the program.
    """
    do_status_output_signal: asyncio.Event = asyncio.Event()

    do_output_to_stdout = flags.logpath == args.LOG_TO_STDOUT_OPTION

    if not do_output_to_stdout:
        tasks.append(
            asyncio.create_task(
                console.console_printer(
                    recorder, flags, do_status_output_signal
                )
            )
        )

    # Initialize event recording.
    recorder.emit_init()

    # Try to parse the flags. Emit one event before and another
    # after flag post processing.
    try:
        if config_file is not None and config_file.is_loaded():
            recorder.emit_load_config(
                config_file.path or "UNKNOWN PATH",
                config_file.default_flags.__dict__,
                config_file.command_line,
            )
        recorder.emit_parse_flags(flags.__dict__)
        flags.validate()
        recorder.emit_parse_flags(flags.__dict__)
    except args.FlagError as e:
        recorder.emit_end(f"Flags are invalid: {e}")
        return 1

    recorder.emit_verbatim_message(
        statusinfo.highlight("Welcome to fx test 🧪\n", style=flags.style)
    )

    # Initialize status printing at this point, if desired.
    if flags.status and not do_output_to_stdout:
        do_status_output_signal.set()
        termout.init()

    # Process and initialize the incoming environment.
    exec_env: environment.ExecutionEnvironment
    try:
        exec_env = environment.ExecutionEnvironment.initialize_from_args(flags)
    except environment.EnvironmentError as e:
        recorder.emit_end(
            f"Failed to initialize environment: {e}\nDid you run fx set?"
        )
        return 1
    recorder.emit_process_env(exec_env.__dict__)

    # Configure file logging based on flags.
    if flags.log and exec_env.log_file:
        output_file: typing.TextIO
        if exec_env.log_to_stdout():
            output_file = sys.stdout
        else:
            output_file = gzip.open(exec_env.log_file, "wt")
        tasks.append(asyncio.create_task(log.writer(recorder, output_file)))
        # TODO(https://fxbug.dev/326214131): Remove log path printing when we support `-pr logpath`.
        recorder.emit_instruction_message(
            f"Logging all output to: {exec_env.log_file}"
        )
        recorder.emit_instruction_message(
            "Use the `--logpath` argument to specify a log location or `--no-log` to disable"
        )

        # For convenience, display the log output path when the program exits.
        # Since the async loop may already be exited at that point, directly
        # print to the console.
        if not exec_env.log_to_stdout():
            atexit.register(
                print,
                statusinfo.dim(
                    f"Output was logged to: {os.path.relpath(exec_env.log_file, os.getcwd())}",
                    style=flags.style,
                ),
            )

    if flags.has_debugger():
        recorder.emit_warning_message(
            "🛑 Debugger integration is currently experimental, follow https://fxbug.dev/319320287 for updates 🛑"
        )

    # Load the list of tests to execute.
    try:
        tests = await load_test_list(recorder, exec_env)
    except Exception as e:
        recorder.emit_end(f"Failed to load tests: {e}")
        return 1

    # Use flags to select which tests to run.
    try:
        mode = selection.SelectionMode.ANY
        if flags.host:
            mode = selection.SelectionMode.HOST
        elif flags.device:
            mode = selection.SelectionMode.DEVICE
        elif flags.only_e2e:
            mode = selection.SelectionMode.E2E
        selections = await selection.select_tests(
            tests,
            flags.selection,
            mode,
            flags.fuzzy,
            recorder=recorder,
            exact_match=flags.exact,
        )
        # Mutate the selections based on the command line flags.
        selections.apply_flags(flags)
        if len(selections.selected_but_not_run) != 0:
            total_count = len(selections.selected) + len(
                selections.selected_but_not_run
            )
            recorder.emit_info_message(
                f"Selected {total_count} tests, but only running {len(selections.selected)} due to flags."
            )
        recorder.emit_test_selections(selections)
    except selection.SelectionError as e:
        recorder.emit_end(f"Selection is invalid: {e}")
        return 1

    # Check that the selected tests are valid.
    try:
        await validate_test_selections(selections, recorder, flags)
    except SelectionValidationError as e:
        recorder.emit_end(str(e))
        return 1

    # Don't actually run any tests if --dry was specified, instead just
    # print which tests were selected and exit.
    if flags.dry:
        recorder.emit_verbatim_message("Selected the following tests:")
        for s in selections.selected:
            recorder.emit_verbatim_message(f"  {s.name()}")
        recorder.emit_instruction_message(
            "\nWill not run any tests, --dry specified"
        )
        recorder.emit_end()
        return 0

    # If enabled, try to build and update the selected tests.
    if flags.build and not await do_build(selections, recorder, exec_env):
        recorder.emit_end("Failed to build.")
        return 1

    if flags.updateifinbase and has_tests_in_base(
        selections, recorder, exec_env
    ):
        status_suffix = (
            "\nStatus output suspended." if termout.is_init() else ""
        )
        recorder.emit_info_message(f"\nBuilding update package.{status_suffix}")
        recorder.emit_instruction_message(
            "Use --no-updateifinbase to skip updating base packages."
        )
        build_return_code = await run_build_with_suspended_output(
            ["build/images/updates"],
            show_output=not exec_env.log_to_stdout(),
        )
        if build_return_code != 0:
            recorder.emit_end(
                f"Failed to build update package ({build_return_code})"
            )
            return 1
        recorder.emit_info_message("\nRunning an OTA before executing tests")
        ota_result = await execution.run_command(
            "fx", "ota", "--no-build", recorder=recorder, print_verbatim=True
        )
        if ota_result is None or ota_result.return_code != 0:
            recorder.emit_warning_message(
                "OTA failed, attempting to run tests anyway"
            )

    # Generate a new test-list.json file based on the built tests.
    try:
        test_list_entries = await generate_test_list(
            exec_env, recorder=recorder
        )
    except (ValueError, RuntimeError) as e:
        recorder.emit_end(f"Failed to generate and load test-list.json: {e}")
        return 1

    try:
        test_list_file.Test.augment_tests_with_info(
            selections.selected, test_list_entries
        )
    except ValueError as e:
        recorder.emit_end(
            f"Generated test-list.json is inconsistent: {e}.\nThis is a bug."
        )
        return 1

    # Don't actually run tests if --list was specified, instead gather the
    # list of test cases for each test and output to the user.
    if flags.list:
        recorder.emit_info_message("Enumerating all test cases...")
        recorder.emit_instruction_message(
            "Will not run any tests, --list specified"
        )
        await enumerate_test_cases(selections, recorder, flags, exec_env)
        recorder.emit_end()
        return 0

    # Finally, run all selected tests.
    if not await run_all_tests(selections, recorder, flags, exec_env):
        recorder.emit_end("Test failures reported")
        return 1

    recorder.emit_end()
    return 0


async def load_test_list(
    recorder: event.EventRecorder, exec_env: environment.ExecutionEnvironment
) -> list[test_list_file.Test]:
    """Load the input files listing tests and parse them into a list of Tests.

    Args:
        recorder (event.EventRecorder): Recorder for events.
        exec_env (environment.ExecutionEnvironment): Environment we run in.

    Raises:
        TestFileError: If the tests.json file is invalid.
        DataParseError: If data could not be deserialized from JSON input.
        JSONDecodeError: If a JSON file fails to parse.
        IOError: If a file fails to open.
        ValueError: If the tests.json and test-list.json files are
            incompatible for some reason.

    Returns:
        list[test_list_file.Test]: List of available tests to execute.
    """

    # Load the tests.json file.
    try:
        parse_id = recorder.emit_start_file_parsing(
            exec_env.relative_to_root(exec_env.test_json_file),
            exec_env.test_json_file,
        )
        test_file_entries: list[
            tests_json_file.TestEntry
        ] = tests_json_file.TestEntry.from_file(exec_env.test_json_file)
        recorder.emit_test_file_loaded(
            test_file_entries, exec_env.test_json_file
        )
        recorder.emit_end(id=parse_id)
    except (tests_json_file.TestFileError, json.JSONDecodeError, IOError) as e:
        recorder.emit_end("Failed to parse: " + str(e), id=parse_id)
        raise e

    # Wrap contents of test.json in PartialTest, which supports matching but
    # still needs a lazily created test-list.json to be a full Test.
    try:
        tests = list(map(test_list_file.Test, test_file_entries))
        return tests
    except ValueError as e:
        recorder.emit_end(
            f"tests.json and test-list.json are inconsistent: {e}"
        )
        raise e


async def generate_test_list(
    exec_env: environment.ExecutionEnvironment, recorder: event.EventRecorder
) -> dict[str, test_list_file.TestListEntry]:
    with tempfile.TemporaryDirectory() as td:
        out_path = os.path.join(td, "test-list.json")
        result = await execution.run_command(
            "fx",
            "test_list_tool",
            "--build-dir",
            exec_env.out_dir,
            "--input",
            exec_env.test_json_file,
            "--output",
            out_path,
            "--test-components",
            os.path.join(exec_env.out_dir, "test_components.json"),
            recorder=recorder,
        )
        if result is None or result.return_code != 0:
            if result is not None:
                suffix = f":\n{result.stdout}\n{result.stderr}"
            raise RuntimeError(
                f"Could not generate a new test-list.json{suffix}"
            )

        # Load the generated test-list.json file.
        try:
            parse_id = recorder.emit_start_file_parsing(
                exec_env.relative_to_root(exec_env.test_list_file),
                exec_env.test_list_file,
            )
            test_list_entries = test_list_file.TestListFile.entries_from_file(
                exec_env.test_list_file
            )
            recorder.emit_end(id=parse_id)
            return test_list_entries
        except (dataparse.DataParseError, json.JSONDecodeError, IOError) as e:
            raise e


class SelectionValidationError(Exception):
    """A problem occurred when validating test selections.

    The message contains a human-readable explanation of the problem.
    """


async def validate_test_selections(
    selections: selection_types.TestSelections,
    recorder: event.EventRecorder,
    flags: args.Flags,
) -> None:
    """Validate the selections matched from tests.json.

    Args:
        selections (TestSelections): The selection output to validate.
        recorder (event.EventRecorder): An event recorder to write useful messages to.

    Raises:
        SelectionValidationError: If the selections are invalid.
    """

    missing_groups: list[selection_types.MatchGroup] = []

    for group, matches in selections.group_matches:
        if not matches:
            missing_groups.append(group)

    if missing_groups:
        recorder.emit_warning_message(
            "\nCould not find any tests to run for at least one set of arguments you provided."
        )
        recorder.emit_info_message(
            "\nMake sure this test is transitively in your 'fx set' arguments."
        )
        recorder.emit_info_message(
            "See https://fuchsia.dev/fuchsia-src/development/testing/faq for more information."
        )

        if flags.show_suggestions:

            def suggestion_args(
                arg: str, threshold: float | None = None
            ) -> list[str]:
                name = "fx"
                suggestion_args = [
                    "search-tests",
                    f"--max-results={flags.suggestion_count}",
                    "--color" if flags.style else "--no-color",
                    arg,
                ]
                if threshold is not None:
                    suggestion_args += ["--threshold", str(threshold)]
                return [name] + suggestion_args

            arg_threshold_pairs = []
            for group in missing_groups:
                # Create pairs of a search string and threshold.
                # Thresholds depend on the number of arguments joined.
                # We have only a single search field, so we concatenate
                # the names into one big group.  To correct for lower
                # match thresholds due to this union, we adjust the
                # threshold when there is more than a single value to
                # match against.
                all_args = group.names.union(group.components).union(
                    group.packages
                )
                arg_threshold_pairs.append(
                    (
                        ",".join(list(all_args)),
                        (
                            max(0.4, 0.9 - len(all_args) * 0.05)
                            if len(all_args) > 1
                            else None
                        ),
                    ),
                )

            outputs = await run_commands_in_parallel(
                [
                    suggestion_args(arg_pair[0], arg_pair[1])
                    for arg_pair in arg_threshold_pairs
                ],
                "Find suggestions",
                recorder=recorder,
                maximum_parallel=10,
            )

            if any([val is None for val in outputs]):
                return

            for group, output in zip(missing_groups, outputs):
                assert output is not None  # Checked above
                recorder.emit_info_message(
                    f"\nFor `{group}`, did you mean any of the following?\n"
                )
                recorder.emit_verbatim_message(output.stdout)

    if missing_groups:
        raise SelectionValidationError(
            "No tests found for the following selections:\n "
            + "\n ".join([str(m) for m in missing_groups])
        )


async def do_build(
    tests: selection_types.TestSelections,
    recorder: event.EventRecorder,
    exec_env: environment.ExecutionEnvironment,
) -> bool:
    """Attempt to build the selected tests.

    Args:
        tests (selection.TestSelections): Tests to attempt to build.
        recorder (event.EventRecorder): Recorder for events.
        exec_env (environment.ExecutionEnvironment): Incoming execution environment.

    Returns:
        bool: True only if the tests were built and published, False otherwise.
    """
    # Labels start with // and end with a toolchain, starting with
    # '('. Both toolchain and '//' need to be omitted for building
    # device tests through fx build.
    label_to_rule = re.compile(r"//([^()]+)\((.+)\)")
    build_targets_by_toolchain: defaultdict[str, list[str]] = defaultdict(list)
    for selection in tests.selected:
        label = selection.build.test.package_label or selection.build.test.label
        match = label_to_rule.match(label)

        if match:
            target = match.group(1)
            toolchain = match.group(2)
            assert isinstance(toolchain, str)

            build_targets_by_toolchain[toolchain].append(f"//{target}")
        else:
            recorder.emit_warning_message(f"Unknown entry {selection}")
            return False

    build_command_line = []
    for key, vals in sorted(list(build_targets_by_toolchain.items())):
        if "fuchsia:" in key:
            # Use --default instead of fuchsia toolchains, otherwise some
            # ZBI tests fail to build.
            build_command_line.append("--default")
        else:
            build_command_line.append(f"--toolchain={key}")
        build_command_line.extend(vals)

    build_id = recorder.emit_build_start(targets=build_command_line)
    recorder.emit_instruction_message("Use --no-build to skip building")

    status_suffix = " Status output suspended." if termout.is_init() else ""
    recorder.emit_info_message(f"\nExecuting build.{status_suffix}")

    await asyncio.sleep(0.1)

    return_code = await run_build_with_suspended_output(
        build_command_line, show_output=not exec_env.log_to_stdout()
    )

    error = None
    if return_code != 0:
        error = f"Build returned non-zero exit code {return_code}"
    if error is not None:
        recorder.emit_end(error, id=build_id)
        return False

    amber_directory = os.path.join(exec_env.out_dir, "amber-files")
    delivery_blob_type = read_delivery_blob_type(exec_env, recorder)
    publish_args = (
        [
            "fx",
            "ffx",
            "repository",
            "publish",
            "--trusted-root",
            os.path.join(amber_directory, "repository/root.json"),
            "--ignore-missing-packages",
            "--time-versioning",
        ]
        + (
            ["--delivery-blob-type", str(delivery_blob_type)]
            if delivery_blob_type is not None
            else []
        )
        + [
            "--package-list",
            os.path.join(exec_env.out_dir, "all_package_manifests.list"),
            amber_directory,
        ]
    )

    output = await execution.run_command(
        *publish_args,
        recorder=recorder,
        parent=build_id,
        print_verbatim=True,
        env={"CWD": exec_env.out_dir},
    )
    if not output:
        error = "Failure publishing packages."
    elif output.return_code != 0:
        error = f"Publish returned non-zero exit code {output.return_code}"
    elif not await post_build_checklist(tests, recorder, exec_env, build_id):
        error = "Post build checklist failed"

    recorder.emit_end(error, id=build_id)

    return error is None


def read_delivery_blob_type(
    exec_env: environment.ExecutionEnvironment,
    recorder: event.EventRecorder,
) -> int | None:
    """Read the delivery blob type from the output directory.

    The delivery_blob_config.json file contains a "type" field that must
    be passed along to package publishing if set.

    This functions attempts to load the file and returns the value of that
    field if set.

    Args:
        exec_env (environment.ExecutionEnvironment): Test execution environment.

    Returns:
        int | None: The delivery blob type, if found. None otherwise.
    """
    expected_path = os.path.join(exec_env.out_dir, "delivery_blob_config.json")
    id = recorder.emit_start_file_parsing(
        "delivery_blob_config.json", expected_path
    )
    if not os.path.isfile(expected_path):
        recorder.emit_end(
            error="Could not find delivery_blob_config.json in output", id=id
        )
        return None

    with open(expected_path) as f:
        val: dict[str, typing.Any] = json.load(f)
        recorder.emit_end(id=id)
        return int(val["type"]) if "type" in val else None


def has_tests_in_base(
    tests: selection_types.TestSelections,
    recorder: event.EventRecorder,
    exec_env: environment.ExecutionEnvironment,
) -> bool:
    base_file = os.path.join(exec_env.out_dir, "base_packages.list")
    parse_id = recorder.emit_start_file_parsing("base_packages.list", base_file)

    manifests: list[str]
    try:
        with open(base_file) as f:
            contents = json.load(f)
        manifests = contents["content"]["manifests"]
    except (IOError, json.JSONDecodeError, KeyError) as e:
        recorder.emit_end(f"Parsing file failed: {e}", id=parse_id)
        raise e

    manifest_ends = {m.split("/")[-1] for m in manifests}
    in_base = [
        name
        for t in tests.selected
        if (name := t.package_name()) in manifest_ends
    ]

    if in_base:
        names = ", ".join(in_base[:3])
        tests_are_in_base_including = (
            "tests are in base, including"
            if len(in_base) > 1
            else "test is in base:"
        )
        recorder.emit_info_message(
            f"\n{len(in_base)} {tests_are_in_base_including} {names}"
        )

    recorder.emit_end(id=parse_id)

    return bool(in_base)


@functools.lru_cache
async def has_device_connected(
    recorder: event.EventRecorder, parent: event.Id | None = None
) -> bool:
    """Check if a device is connected for running target tests.

    Args:
        recorder (event.EventRecorder): Recorder for events.
        parent (event.Id, optional): Parent task ID. Defaults to None.

    Returns:
        bool: True only if a device is available to run target tests.
    """
    output = await execution.run_command(
        "fx", "is-package-server-running", recorder=recorder, parent=parent
    )
    return output is not None and output.return_code == 0


async def run_build_with_suspended_output(
    build_command_line: list[str],
    show_output: bool = True,
) -> int:
    # Allow display to update.
    await asyncio.sleep(0.1)

    if termout.is_init():
        # Clear the status output while we are doing the build.
        termout.write_lines([])

    stdout = None if show_output else subprocess.DEVNULL
    stderr = None if show_output else subprocess.DEVNULL

    return_code = subprocess.call(
        ["fx", "build"] + build_command_line, stdout=stdout, stderr=stderr
    )
    return return_code


async def post_build_checklist(
    tests: selection_types.TestSelections,
    recorder: event.EventRecorder,
    exec_env: environment.ExecutionEnvironment,
    build_id: event.Id,
) -> bool:
    """Perform a number of post-build checks to ensure we are ready to run tests.

    Args:
        tests (selection.TestSelections): Tests selected to run.
        recorder (event.EventRecorder): Recorder for events.
        exec_env (environment.ExecutionEnvironment): Execution environment.
        build_id (event.Id): ID of the build event to use at the parent of any operations executed here.

    Returns:
        bool: True only if post-build checks passed, False otherwise.
    """
    if tests.has_device_test() and await has_device_connected(
        recorder, parent=build_id
    ):
        try:
            if has_tests_in_base(tests, recorder, exec_env):
                recorder.emit_info_message(
                    "Some selected test(s) are in the base package set. Running an OTA."
                )
                output = await execution.run_command(
                    "fx", "ota", recorder=recorder, print_verbatim=True
                )
                if not output or output.return_code != 0:
                    recorder.emit_warning_message("OTA failed")
                    return False
        except IOError as e:
            return False

    return True


async def run_all_tests(
    tests: selection_types.TestSelections,
    recorder: event.EventRecorder,
    flags: args.Flags,
    exec_env: environment.ExecutionEnvironment,
) -> bool:
    """Execute all selected tests.

    Args:
        tests (selection.TestSelections): The selected tests to run.
        recorder (event.EventRecorder): Recorder for events.
        flags (args.Flags): Incoming command flags.
        exec_env (environment.ExecutionEnvironment): Execution environment.

    Returns:
        bool: True only if all tests ran successfully, False otherwise.
    """
    max_parallel = flags.parallel
    if tests.has_device_test() and not await has_device_connected(recorder):
        recorder.emit_warning_message(
            "\nCould not find a running package server."
        )
        recorder.emit_instruction_message(
            "\nYou do not seem to have a package server running, but you have selected at least one device test.\nEnsure that you have `fx serve` running and that you have selected your desired device using `fx set-device`.\n"
        )
        return False

    device_environment: environment.DeviceEnvironment | None = None
    if tests.has_e2e_test():
        device_environment = (
            await execution.get_device_environment_from_exec_env(
                exec_env, recorder=recorder
            )
        )

    test_group = recorder.emit_test_group(len(tests.selected) * flags.count)

    @dataclass
    class ExecEntry:
        """Wrapper for test executions to share a signal for aborting by groups."""

        # The test execution to run.
        exec: execution.TestExecution

        # Signal for aborting the execution of a specific group of tests,
        # including this one.
        abort_group: asyncio.Event

    @dataclass
    class RunState:
        total_running: int = 0
        non_hermetic_running: int = 0
        hermetic_test_queue: asyncio.Queue[ExecEntry] = field(
            default_factory=lambda: asyncio.Queue()
        )
        non_hermetic_test_queue: asyncio.Queue[ExecEntry] = field(
            default_factory=lambda: asyncio.Queue()
        )

    run_condition = asyncio.Condition()
    run_state = RunState()

    for test in tests.selected:
        abort_group = asyncio.Event()

        execs = [
            execution.TestExecution(
                test,
                exec_env,
                flags,
                run_suffix=None if flags.count == 1 else i + 1,
                device_env=(
                    None if not test.is_e2e_test() else device_environment
                ),
            )
            for i in range(flags.count)
        ]

        for exec in execs:
            if exec.is_hermetic():
                run_state.hermetic_test_queue.put_nowait(
                    ExecEntry(exec, abort_group)
                )
            else:
                run_state.non_hermetic_test_queue.put_nowait(
                    ExecEntry(exec, abort_group)
                )

    tasks = []

    abort_all_tests_event = asyncio.Event()
    test_failure_observed: bool = False

    maybe_debugger: subprocess.Popen[bytes] | None = None
    debugger_ready: asyncio.Condition = asyncio.Condition()

    if flags.has_debugger():

        async def on_debugger_ready() -> None:
            # TODO(b/329317913): Emit a debugger event here.
            async with debugger_ready:
                debugger_ready.notify_all()

        maybe_debugger = debugger.spawn(
            tests.selected,
            on_debugger_ready,
            break_on_failure=flags.break_on_failure,
            breakpoints=flags.breakpoints,
        )

    async def test_executor() -> None:
        nonlocal test_failure_observed
        to_run: ExecEntry
        was_non_hermetic: bool = False

        while True:
            async with run_condition:
                # Wait until we are allowed to try to run a test.
                while run_state.total_running == max_parallel:
                    await run_condition.wait()

                # If we should not execute any more tests, quit.
                if abort_all_tests_event.is_set():
                    return

                if (
                    run_state.non_hermetic_running == 0
                    and not run_state.non_hermetic_test_queue.empty()
                ):
                    to_run = run_state.non_hermetic_test_queue.get_nowait()
                    run_state.non_hermetic_running += 1
                    was_non_hermetic = True
                elif run_state.hermetic_test_queue.empty():
                    return
                else:
                    to_run = run_state.hermetic_test_queue.get_nowait()
                    was_non_hermetic = False
                run_state.total_running += 1

            test_suite_id = recorder.emit_test_suite_started(
                to_run.exec.name(), not was_non_hermetic, parent=test_group
            )
            status: event.TestSuiteStatus
            message: str | None = None
            try:
                if not to_run.abort_group.is_set():
                    # Only run if this group was not already aborted.
                    command_line = " ".join(to_run.exec.command_line())
                    recorder.emit_instruction_message(
                        f"Command: {command_line}"
                    )

                    # Wait for the command completion and any other signal that
                    # means we should stop running the test.
                    done, pending = await asyncio.wait(
                        [
                            asyncio.create_task(
                                to_run.exec.run(
                                    recorder,
                                    flags,
                                    test_suite_id,
                                    timeout=flags.timeout,
                                )
                            ),
                            asyncio.create_task(abort_all_tests_event.wait()),
                            asyncio.create_task(to_run.abort_group.wait()),
                        ],
                        return_when=asyncio.FIRST_COMPLETED,
                    )
                    for r in pending:
                        # Cancel pending tasks.
                        # This must happen before we throw exceptions to ensure
                        # tasks are properly cleaned up.
                        r.cancel()
                    if pending:
                        # Propagate cancellations
                        await asyncio.wait(pending)
                    for r in done:
                        # Re-throw exceptions.
                        r.result()

                if abort_all_tests_event.is_set():
                    status = event.TestSuiteStatus.ABORTED
                    message = "Test suite aborted due to another failure"
                elif to_run.abort_group.is_set():
                    status = event.TestSuiteStatus.ABORTED
                    message = "Aborted re-runs due to another failure"
                else:
                    status = event.TestSuiteStatus.PASSED
            except execution.TestCouldNotRun as e:
                status = event.TestSuiteStatus.SKIPPED
                message = str(e)
            except execution.TestSkipped as e:
                status = event.TestSuiteStatus.SKIPPED
                message = str(e)
            except (execution.TestTimeout, execution.TestFailed) as e:
                if isinstance(e, execution.TestTimeout):
                    status = event.TestSuiteStatus.TIMEOUT
                    # Abort other tests in this group.
                    to_run.abort_group.set()
                else:
                    status = event.TestSuiteStatus.FAILED
                test_failure_observed = True
                if flags.fail:
                    # Abort all other running tests, dropping through to the
                    # following run state code to trigger any waiting executors.
                    abort_all_tests_event.set()
            finally:
                recorder.emit_test_suite_ended(test_suite_id, status, message)

            async with run_condition:
                run_state.total_running -= 1
                if was_non_hermetic:
                    run_state.non_hermetic_running -= 1
                run_condition.notify()

    # Wait for the debugger to signal that it is ready.
    if maybe_debugger is not None:
        async with debugger_ready:
            await debugger_ready.wait()

    for _ in range(max_parallel):
        tasks.append(asyncio.create_task(test_executor()))

    await asyncio.wait(tasks)

    if maybe_debugger is not None:
        # Close the fifo to signal zxdb to close and reset stdout to /dev/null so termout doesn't fail its cleanup.
        sys.stdout.close()
        sys.stdout = open(os.devnull, "w")

        # This is a synchronous wait and we don't want to block the event loop, so run it in the
        # default thread executor.
        loop = asyncio.get_event_loop()
        await loop.run_in_executor(None, maybe_debugger.wait)

    recorder.emit_end(id=test_group)

    return not test_failure_observed


async def enumerate_test_cases(
    tests: selection_types.TestSelections,
    recorder: event.EventRecorder,
    flags: args.Flags,
    exec_env: environment.ExecutionEnvironment,
) -> None:
    # Get the set of test executions that support enumeration.
    executions = [
        e
        for t in tests.selected
        if (
            e := execution.TestExecution(t, exec_env, flags)
        ).enumerate_cases_command_line()
        is not None
    ]

    wont_enumerate_count = len(tests.selected) - len(executions)

    outputs = await run_commands_in_parallel(
        [
            cmd_line
            for e in executions
            if (cmd_line := e.enumerate_cases_command_line()) is not None
        ],
        group_name="Enumerate test cases",
        recorder=recorder,
        maximum_parallel=8,
    )

    assert len(outputs) == len(executions)

    if wont_enumerate_count > 0:
        recorder.emit_info_message(
            f"\n{wont_enumerate_count:d} tests do not support enumeration"
        )

    failed_enumeration_names = []
    for output, exec in zip(outputs, executions):
        if output is None or output.return_code != 0:
            failed_enumeration_names.append(exec.name())
            continue
        recorder.emit_enumerate_test_cases(
            exec.name(), list(output.stdout.splitlines())
        )

    if failed_enumeration_names:
        recorder.emit_info_message(
            f"{len(failed_enumeration_names)} tests could not be enumerated"
        )


async def run_commands_in_parallel(
    commands: list[list[str]],
    group_name: str,
    recorder: event.EventRecorder | None = None,
    maximum_parallel: int | None = None,
) -> list[command.CommandOutput | None]:
    assert recorder

    parent = recorder.emit_event_group(group_name, queued_events=len(commands))
    output: list[command.CommandOutput | None] = [None] * len(commands)
    in_progress: typing.Set[asyncio.Task[None]] = set()

    index = 0

    def can_add() -> bool:
        nonlocal index
        return index < len(commands) and (
            maximum_parallel is None or len(in_progress) < maximum_parallel
        )

    while index < len(commands) or in_progress:
        while can_add():

            async def set_index(i: int) -> None:
                output[i] = await execution.run_command(
                    *commands[i], recorder=recorder, parent=parent
                )

            in_progress.add(asyncio.create_task(set_index(index)))
            index += 1

        _, in_progress = await asyncio.wait(
            in_progress, return_when="FIRST_COMPLETED"
        )

    recorder.emit_end(id=parent)

    return output


if __name__ == "__main__":
    main()
