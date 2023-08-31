# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from __future__ import annotations

import asyncio
import atexit
from dataclasses import dataclass
from dataclasses import field
import functools
import gzip
import json
import os
import random
import re
import sys
import typing

import args
import console
import dataparse
import environment
import event
import execution
import log
import selection
import statusinfo
import termout
import test_list_file
import tests_json_file
import util.command as command
import util.signals


def main():
    # Main entrypoint.
    # Set up the event loop to catch termination signals (i.e. Ctrl+C), and
    # cancel the main task when they are received.
    fut = asyncio.ensure_future(async_main_wrapper(args.parse_args()))
    util.signals.register_on_terminate_signal(fut.cancel)
    try:
        loop = asyncio.get_event_loop()
        loop.run_until_complete(fut)
        sys.exit(fut.result())
    except asyncio.CancelledError:
        print("\n\nReceived interrupt, exiting")
        sys.exit(1)


async def async_main_wrapper(
    flags: args.Flags, recorder: event.EventRecorder | None = None
) -> int:
    """Wrapper for the main logic of fx test.

    This wrapper creates a list containing tasks that must be
    awaited before the program exits. The main logic may add tasks to this
    list during execution, and then return the intended status code.

    Args:
        flags (args.Flags): Flags to pass into the main function.
        recorder (event.EventRecorder | None, optional): If set,
            use this event recorder. Used for testing.

    Returns:
        The return code of the program.
    """
    tasks: typing.List[asyncio.Task] = []
    if recorder is None:
        recorder = event.EventRecorder()

    ret = await async_main(flags, tasks, recorder)

    try:
        await asyncio.wait_for(asyncio.wait(tasks), timeout=5)
    except asyncio.TimeoutError:
        print(
            "\n\nTimed out waiting for tasks to exit, terminating...\n",
            file=sys.stderr,
        )
    return ret


async def async_main(
    flags: args.Flags, tasks: typing.List[asyncio.Task], recorder: event.EventRecorder
) -> int:
    """Main logic of fx test.

    Args:
        flags (args.Flags): Flags controlling the behavior of fx test.
        tasks (List[asyncio.Tasks]): List to add tasks to that must be awaited before termination.
        recorder (event.Recorder): The recorder for events.

    Returns:
        The return code of the program.
    """
    do_status_output_signal: asyncio.Event = asyncio.Event()

    tasks.append(
        asyncio.create_task(
            console.console_printer(recorder, flags, do_status_output_signal)
        )
    )

    # Initialize event recording.
    recorder.emit_init()

    info_first_line = (
        "✅ You are using the new fx test, which is currently in Fishfood ✅"
    )
    info_block = """See details here: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/scripts/fxtest/rewrite
To go back to the old fx test, use `fx --enable=legacy_fxtest test`, and please file a bug under b/293917801.
"""

    recorder.emit_info_message(info_first_line)
    recorder.emit_instruction_message(info_block)

    # Try to parse the flags. Emit one event before and another
    # after flag post processing.
    try:
        recorder.emit_parse_flags(flags.__dict__)
        flags.validate()
        recorder.emit_parse_flags(flags.__dict__)
    except args.FlagError as e:
        recorder.emit_end(f"Flags are invalid: {e}")
        return 1

    # Initialize status printing at this point, if desired.
    if flags.status:
        do_status_output_signal.set()
        termout.init()

    # Process and initialize the incoming environment.
    exec_env: environment.ExecutionEnvironment
    try:
        exec_env = environment.ExecutionEnvironment.initialize_from_args(flags)
    except environment.EnvironmentError as e:
        recorder.emit_end(f"Failed to initialize environment: {e}\nDid you run fx set?")
        return 1
    recorder.emit_process_env(exec_env.__dict__)

    # Configure file logging based on flags.
    if flags.log and exec_env.log_file:
        tasks.append(
            asyncio.create_task(
                log.writer(recorder, gzip.open(exec_env.log_file, "wt"))
            )
        )
        recorder.emit_instruction_message(f"Logging all output to: {exec_env.log_file}")
        recorder.emit_instruction_message(
            "Use the `--logpath` argument to specify a log location or `--no-log` to disable\n"
        )

        # For convenience, display the log output path when the program exits.
        # Since the async loop may already be exited at that point, directly
        # print to the console.
        atexit.register(
            print,
            statusinfo.dim(
                f"Output was logged to: {os.path.relpath(exec_env.log_file, os.getcwd())}",
                style=flags.style,
            ),
        )

    # Print a message for users who want to know how to see all test output.
    if not flags.output:
        recorder.emit_instruction_message(
            f"To show all output, specify the `-o/--output` flag."
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
        selections = selection.select_tests(
            tests,
            flags.selection,
            mode,
            flags.fuzzy,
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

    # If desired, we randomize the execution order of tests.
    # Otherwise, they are run in the order they appear in the tests.json file.
    if flags.random:
        random.shuffle(selections.selected)

    # Don't actually run any tests if --dry was specified, instead just
    # print which tests were selected and exit.
    if flags.dry:
        recorder.emit_info_message("Selected the following tests:")
        for s in selections.selected:
            recorder.emit_info_message(f"  {s.info.name}")
        recorder.emit_instruction_message("\nWill not run any tests, --dry specified")
        recorder.emit_end()
        return 0

    # If enabled, try to build and update the selected tests.
    if flags.build and not await do_build(selections, recorder, exec_env):
        recorder.emit_end("Failed to build.")
        return 1

    # Finally, run all selected tests.
    if not await run_all_tests(selections, recorder, flags, exec_env):
        recorder.emit_end("Test failures reported")
        return 1

    recorder.emit_end()
    return 0


async def load_test_list(
    recorder: event.EventRecorder, exec_env: environment.ExecutionEnvironment
) -> typing.List[test_list_file.Test]:
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
        typing.List[test_list_file.Test]: List of available tests to execute.
    """

    # Load the tests.json file.
    try:
        parse_id = recorder.emit_start_file_parsing(
            exec_env.relative_to_root(exec_env.test_json_file), exec_env.test_json_file
        )
        test_file_entries: typing.List[
            tests_json_file.TestEntry
        ] = tests_json_file.TestEntry.from_file(exec_env.test_json_file)
        recorder.emit_test_file_loaded(test_file_entries, exec_env.test_json_file)
        recorder.emit_end(id=parse_id)
    except (tests_json_file.TestFileError, json.JSONDecodeError, IOError) as e:
        recorder.emit_end("Failed to parse: " + str(e), id=parse_id)
        raise e

    # Load the test-list.json file.
    try:
        parse_id = recorder.emit_start_file_parsing(
            exec_env.relative_to_root(exec_env.test_list_file), exec_env.test_list_file
        )
        test_list_entries = test_list_file.TestListFile.entries_from_file(
            exec_env.test_list_file
        )
        recorder.emit_end(id=parse_id)
    except (dataparse.DataParseError, json.JSONDecodeError, IOError) as e:
        recorder.emit_end("Failed to parse: " + str(e), id=parse_id)
        raise e

    # Join the contents of the two files and return it.
    try:
        tests = test_list_file.Test.join_test_descriptions(
            test_file_entries, test_list_entries
        )
        return tests
    except ValueError as e:
        recorder.emit_end(f"tests.json and test-list.json are inconsistent: {e}")
        raise e


class SelectionValidationError(Exception):
    """A problem occurred when validating test selections.

    The message contains a human-readable explanation of the problem.
    """


async def validate_test_selections(
    selections: selection.TestSelections,
    recorder: event.EventRecorder,
    flags: args.Flags,
):
    """Validate the selections matched from tests.json.

    Args:
        selections (TestSelections): The selection output to validate.
        recorder (event.EventRecorder): An event recorder to write useful messages to.

    Raises:
        SelectionValidationError: If the selections are invalid.
    """

    missing_groups: typing.List[selection.MatchGroup] = []

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
            ) -> typing.List[str]:
                name = "fx"
                suggestion_args = [
                    "search-tests",
                    f"--max-results={flags.suggestion_count}",
                    arg,
                ]
                if threshold is not None:
                    suggestion_args += ["--threshold", str(threshold)]
                if not flags.style:
                    suggestion_args += ["--no-color"]
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
                all_args = group.names.union(group.components).union(group.packages)
                arg_threshold_pairs.append(
                    (
                        ",".join(list(all_args)),
                        max(0.4, 0.9 - len(all_args) * 0.05)
                        if len(all_args) > 1
                        else None,
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
                return False

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
    tests: selection.TestSelections,
    recorder: event.EventRecorder,
    exec_env: environment.ExecutionEnvironment,
) -> bool:
    """Attempt to build the selected tests.

    Args:
        tests (selection.TestSelections): Tests to attempt to build.
        recorder (event.EventRecorder): Recorder for events.
        exec_env (environment.ExecutionEnvironment): Incoming execution environment.

    Returns:
        bool: True only if the tests were build and published, False otherwise.
    """
    label_to_rule = re.compile(r"//([^()]+)\(")
    build_command_line = []
    for selection in tests.selected:
        label = selection.build.test.package_label or selection.build.test.label
        path = selection.build.test.path
        if path is not None:
            # Host tests are built by output name.
            build_command_line.append(path)
        elif label:
            # Other tests are built by label content, without toolchain.
            match = label_to_rule.match(label)
            if match:
                build_command_line.append(match.group(1))
        else:
            recorder.emit_warning_message(f"Unknown entry {selection}")
            return False

    build_id = recorder.emit_build_start(targets=build_command_line)
    recorder.emit_instruction_message("Use --no-build to skip building")

    output = await execution.run_command(
        "fx",
        *(["build"] + build_command_line),
        recorder=recorder,
        parent=build_id,
        print_verbatim=True,
    )

    error = None
    if not output:
        error = "Failure running build"
    elif output.return_code != 0:
        error = f"Build returned non-zero exit code {output.return_code}"

    if error is not None:
        recorder.emit_end(error, id=build_id)
        return False

    amber_directory = os.path.join(exec_env.out_dir, "amber-files")
    publish_args = [
        "fx",
        "ffx",
        "repository",
        "publish",
        "--trusted-root",
        os.path.join(amber_directory, "repository/root.json"),
        "--ignore-missing-packages",
        "--time-versioning",
        "--package-list",
        os.path.join(exec_env.out_dir, "all_package_manifests.list"),
        amber_directory,
    ]

    output = await execution.run_command(
        *publish_args, recorder=recorder, parent=build_id, print_verbatim=True
    )
    if not output:
        error = "Failure publishing packages."
    elif output.return_code != 0:
        error = f"Publish returned non-zero exit code {output.return_code}"
    elif not await post_build_checklist(tests, recorder, exec_env, build_id):
        error = "Post build checklist failed"

    recorder.emit_end(error, id=build_id)

    return error is None


def has_tests_in_base(
    tests: selection.TestSelections,
    recorder: event.EventRecorder,
    exec_env: environment.ExecutionEnvironment,
) -> bool:
    # TODO(b/291144505): This logic was ported from update-if-in-base, but it appears to be wrong.
    # BUG(b/291144505): Fix this.
    base_file = os.path.join(exec_env.out_dir, "base_packages.list")
    parse_id = recorder.emit_start_file_parsing("base_packages.list", base_file)

    contents = None
    try:
        with open(base_file) as f:
            contents = f.read()
    except IOError as e:
        recorder.emit_end(f"Parsing file failed: {e}", id=parse_id)
        raise e

    ret = any(
        [
            t.build.test.package_url is not None
            and t.build.test.package_url in contents
            for t in tests.selected
        ]
    )

    recorder.emit_end(id=parse_id)

    return ret


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


async def post_build_checklist(
    tests: selection.TestSelections,
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
    tests: selection.TestSelections,
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
        recorder.emit_warning_message("\nCould not find a running package server.")
        recorder.emit_instruction_message(
            "\nYou do not seem to have a package server running, but you have selected at least one device test.\nEnsure that you have `fx serve` running and that you have selected your desired device using `fx set-device`.\n"
        )
        return False

    test_group = recorder.emit_test_group(len(tests.selected))

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

    limit_remaining = len(tests.selected) if flags.limit is None else flags.limit
    for test in tests.selected:
        if limit_remaining <= 0:
            recorder.emit_warning_message(
                f"\nOnly running {flags.limit} tests out of {len(tests.selected)} due to --limit"
            )
            break

        abort_group = asyncio.Event()

        execs = [
            execution.TestExecution(
                test,
                exec_env,
                flags,
                run_suffix=None if flags.count == 1 else i + 1,
            )
            for i in range(flags.count)
        ]

        for exec in execs:
            if exec.is_hermetic():
                run_state.hermetic_test_queue.put_nowait(ExecEntry(exec, abort_group))
            else:
                run_state.non_hermetic_test_queue.put_nowait(
                    ExecEntry(exec, abort_group)
                )
        limit_remaining -= 1

    tasks = []

    abort_all_tests_event = asyncio.Event()
    test_failure_observed: bool = False

    async def test_executor():
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
            message: typing.Optional[str] = None
            try:
                if not to_run.abort_group.is_set():
                    # Only run if this group was not already aborted.
                    command_line = " ".join(to_run.exec.command_line())
                    recorder.emit_instruction_message(f"Command: {command_line}")

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

    for _ in range(max_parallel):
        tasks.append(asyncio.create_task(test_executor()))

    await asyncio.wait(tasks)

    recorder.emit_end(id=test_group)

    return not test_failure_observed


async def run_commands_in_parallel(
    commands: typing.List[typing.List[str]],
    group_name: str,
    recorder: typing.Optional[event.EventRecorder] = None,
    maximum_parallel: typing.Optional[int] = None,
) -> typing.List[typing.Optional[command.CommandOutput]]:
    assert recorder

    parent = recorder.emit_event_group(group_name, queued_events=len(commands))
    output: typing.List[typing.Optional[command.CommandOutput]] = [None] * len(commands)
    in_progress: typing.Set[asyncio.Task] = set()

    index = 0

    def can_add() -> bool:
        nonlocal index
        return index < len(commands) and (
            maximum_parallel is None or len(in_progress) < maximum_parallel
        )

    while index < len(commands) or in_progress:
        while can_add():

            async def set_index(i: int):
                output[i] = await execution.run_command(
                    *commands[i], recorder=recorder, parent=parent
                )

            in_progress.add(asyncio.create_task(set_index(index)))
            index += 1

        _, in_progress = await asyncio.wait(in_progress)

    recorder.emit_end(id=parent)

    return output


if __name__ == "__main__":
    main()
