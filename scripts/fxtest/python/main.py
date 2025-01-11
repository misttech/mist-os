# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from __future__ import annotations

import argparse
import asyncio
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
import time
import typing

import async_utils.command as command
import async_utils.signals as signals
import statusinfo
import termout

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
import test_list_file
import tests_json_file


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

    replay_mode: bool = False

    # Special utility mode handling
    if real_flags.is_replay():
        assert_no_selection(real_flags, "-pr replay")
        replay_mode = True
    elif real_flags.previous is not None:
        sys.exit(do_process_previous(real_flags))

    # No special modes, proceed with async execution.
    fut = asyncio.ensure_future(
        async_main_wrapper(
            real_flags, config_file=config_file, replay_mode=replay_mode
        )
    )
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
    replay_mode: bool = False,
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
        replay_mode (bool, optional): If set, load and replay the most recent log
            instead of running tests.

    Returns:
        The return code of the program.
    """
    tasks: list[asyncio.Task[None]] = []
    if recorder is None:
        recorder = event.EventRecorder()

    end_execution_request_event = asyncio.Event()
    termination_callback_event = asyncio.Event()

    wrapper = AsyncMain(
        flags,
        tasks,
        recorder,
        end_execution_request_event,
        termination_callback_event,
        config_file,
        replay_mode,
    )

    wrapper_task = asyncio.Task(wrapper.main())

    def terminate_handler() -> None:
        end_execution_request_event.set()
        signals.unregister_all_termination_signals()

        def kill_handler() -> None:
            """Immediately cancel and await remaining tasks."""
            wrapper_task.cancel()
            for task in tasks:
                task.cancel()
            print(
                statusinfo.error_highlight(
                    "\nImmediately stopping execution and exiting...",
                    style=flags.style,
                ),
                file=sys.stderr,
            )

        signals.register_on_terminate_signal(kill_handler)

    signals.register_on_terminate_signal(terminate_handler)

    async def terminate_callback_handler() -> None:
        await termination_callback_event.wait()
        wrapper_task.cancel()

    terminate_callback_task = asyncio.Task(terminate_callback_handler())

    try:
        ret = await wrapper_task
    except asyncio.CancelledError:
        ret = 1
    finally:
        terminate_callback_task.cancel()

    if not tasks:
        # Nothing to clean up, return.
        return ret

    # Ensure that queued tasks are cleaned up before returning.
    to_wait = asyncio.Task(asyncio.wait(tasks), name="Drain tasks")
    timeout_seconds = 5
    try:
        await asyncio.wait_for(to_wait, timeout=timeout_seconds)
    except asyncio.TimeoutError:
        print(
            f"\n\nWaiting for tasks to complete for longer than {timeout_seconds} seconds...\n",
            file=sys.stderr,
        )
        wrapper_task.cancel()
        to_wait.cancel()
    except asyncio.CancelledError:
        pass

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
    elif flags.previous is args.PrevOption.ARTIFACT_PATH:
        env = environment.ExecutionEnvironment.initialize_from_args(
            flags, create_log_file=False
        )
        log_source = log.LogSource.from_env(env)
        for element in log_source.read_log():
            if (warning := element.warning) is not None:
                print(f"WARNING: {warning}", file=sys.stderr)
                continue
            if (error := element.error) is not None:
                print(f"ERROR: {error}", file=sys.stderr)
                return 1
            if (event := element.log_event) is None:
                continue
            if event.payload is None:
                continue
            if (artifact_path := event.payload.artifact_directory_path) is None:
                continue
            if artifact_path == "":
                print(
                    "ERROR: The previous run did not specify --artifact-output-directory. Run again with that flag set to get the path.",
                    file=sys.stderr,
                )
                return 1
            if not os.path.isdir(artifact_path):
                print(
                    "ERROR: The artifact directory is missing, it may have been deleted.",
                    file=sys.stderr,
                )
                return 1
            print(artifact_path)
            return 0
        print(
            "ERROR: The previous run is missing an artifact output directory. The log may be corrupt or incomplete.",
            file=sys.stderr,
        )
        return 1
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
    if log.pretty_print(log.LogSource.from_env(env)):
        return 0
    else:
        return 1


async def do_replay_log(
    flags: args.Flags, event_signal_for_printer: asyncio.Event
) -> int:
    env = environment.ExecutionEnvironment.initialize_from_args(
        flags, create_log_file=False
    )

    content: list[event.Event] = []
    for log_element in log.LogSource.from_env(env).read_log():
        if log_element.log_event is not None:
            content.append(log_element.log_event)
        elif log_element.warning is not None:
            print(log_element.warning, file=sys.stderr)
        elif log_element.error is not None:
            print(log_element.error, file=sys.stderr)
            return 1

    real_monotonic = time.monotonic()
    if not content:
        print("Log file was empty.", file=sys.stderr)
        return 1
    if content[0].id != event.GLOBAL_RUN_ID:
        print(
            "BUG: Invalid log file, expected to start with a global run identifier.",
            file=sys.stderr,
        )
        return 1
    log_monotonic_start = content[0].timestamp

    def simulated_monotonic_time() -> float:
        real_offset = time.monotonic() - real_monotonic
        return log_monotonic_start + real_offset * flags.replay_speed

    async def wait_until_simulated_time(
        desired_simulated_timestamp: float,
    ) -> None:
        current_simulated_timestamp = simulated_monotonic_time()
        if current_simulated_timestamp < desired_simulated_timestamp:
            simulated_diff = (
                desired_simulated_timestamp - current_simulated_timestamp
            )
            real_diff = simulated_diff / flags.replay_speed
            await asyncio.sleep(real_diff)

    recorder = event.EventRecorder()
    output_future = console.ConsoleOutput(
        monotonic_time_source=simulated_monotonic_time
    ).console_printer(recorder, flags, event_signal_for_printer)

    async def pump_events() -> None:
        test_suite_ids: set[event.Id] = set()
        test_suite_child_ids: set[event.Id] = set()
        for event in content:
            # Wait until it is time to emit this event.
            await wait_until_simulated_time(event.timestamp)

            # Keep track of which programs are part of test suites.
            # We need to override the "print_verbatim" parameter
            # to match the current setting of output for those events.
            if event.parent in test_suite_ids and event.id is not None:
                test_suite_child_ids.add(event.id)
            if (
                event.payload is not None
                and event.payload.test_suite_started is not None
                and event.id is not None
            ):
                test_suite_ids.add(event.id)
            if (
                event.id in test_suite_child_ids
                and event.payload is not None
                and (output_payload := event.payload.program_output) is not None
            ):
                output_payload.print_verbatim = flags.output
            recorder._emit(event)
        recorder.end()

    tasks = [
        asyncio.create_task(pump_events()),
        asyncio.create_task(output_future),
    ]
    await asyncio.wait(tasks)

    print(
        f"\nReplay complete: {len(content)} events from {env.get_most_recent_log()}"
    )

    return 0


class AsyncMain:
    _ALL_PACKAGE_MANIFESTS_PATH = [
        "all_package_manifests.list",
    ]

    _PACKAGE_MANIFESTS_FROM_METADATA_PATH = [
        "gen",
        "build",
        "images",
        "updates",
        "package_manifests_from_metadata.list.package_metadata",
    ]

    def __init__(
        self,
        flags: args.Flags,
        tasks: list[asyncio.Task[None]],
        recorder: event.EventRecorder,
        end_execution_request_event: asyncio.Event,
        termination_callback_event: asyncio.Event,
        config_file: config.ConfigFile | None = None,
        replay_mode: bool = False,
    ):
        """Wrapper for main logic of fx test

        Args:
            flags (args.Flags): Flags controlling the behavior of fx test.
            tasks (List[asyncio.Tasks]): List to add tasks to that must be awaited before termination.
            recorder (event.Recorder): The recorder for events.
            end_execution_request_event (asyncio.Event): Set by caller to gracefully stop execution.
            termination_callback_event (asyncio.Event): Set by callee to instruct caller to terminate execution.
            config_file (config.ConfigFile, optional): The loaded config, if one was set.
            replay_mode (bool, optional): If set, load and replay the most recent log instead
        """
        self._flags = flags
        self._tasks = tasks
        self._recorder = recorder
        self._config_file = config_file
        self._replay_mode = replay_mode
        self._exec_env: environment.ExecutionEnvironment | None = None
        self._end_execution_request_event = end_execution_request_event
        self._termination_callback_event = termination_callback_event

    async def main(self) -> int:
        """Execute the fx test command through this wrapper.

        Returns:
            The return code of the program.
        """
        do_status_output_signal: asyncio.Event = asyncio.Event()
        do_output_to_stdout = self._flags.logpath == args.LOG_TO_STDOUT_OPTION
        recorder = self._recorder
        flags = self._flags

        async def immediate_exit_handler() -> None:
            await self._end_execution_request_event.wait()
            recorder.emit_warning_message("Interrupt received, terminating.")
            self._termination_callback_event.set()
            recorder.emit_end(error="Terminated due to interrupt")

        # Until the point this task is canceled below, any exit request will request
        # an immediate termination of the command.
        _immediate_exit_task = asyncio.Task(immediate_exit_handler())

        if not do_output_to_stdout and not self._replay_mode:
            self._tasks.append(
                asyncio.create_task(
                    console.ConsoleOutput().console_printer(
                        recorder, flags, do_status_output_signal
                    )
                )
            )

        # Initialize event recording.
        if not self._replay_mode:
            recorder.emit_init()

        # Try to parse the flags. Emit one event before and another
        # after flag post processing.
        try:
            if self._config_file is not None and self._config_file.is_loaded():
                if not self._replay_mode:
                    recorder.emit_load_config(
                        self._config_file.path or "UNKNOWN PATH",
                        self._config_file.default_flags.__dict__,
                        self._config_file.command_line,
                    )
                recorder.emit_parse_flags(flags.__dict__)
            flags.validate()
            if not self._replay_mode:
                recorder.emit_parse_flags(flags.__dict__)
        except args.FlagError as e:
            if not self._replay_mode:
                recorder.emit_end(f"Flags are invalid: {e}")
            else:
                print(f"Failed to load real flags")
            return 1

        recorder.emit_verbatim_message(
            statusinfo.highlight("Welcome to fx test 🧪\n", style=flags.style)
        )

        # Initialize status printing at this point, if desired.
        if flags.status and not do_output_to_stdout:
            do_status_output_signal.set()
            termout.init()

        if self._replay_mode:
            return await do_replay_log(flags, do_status_output_signal)

        # Process and initialize the incoming environment.
        exec_env: environment.ExecutionEnvironment
        try:
            exec_env = environment.ExecutionEnvironment.initialize_from_args(
                flags
            )
        except environment.EnvironmentError as e:
            recorder.emit_end(
                f"Failed to initialize environment: {e}\nDid you run fx set?"
            )
            return 1
        self._exec_env = exec_env
        recorder.emit_process_env(exec_env)

        flags.update_artifacts_directory_with_out_path(
            os.path.abspath(exec_env.out_dir)
        )

        if (
            flags.artifact_output_directory
            and os.path.exists(flags.artifact_output_directory)
            and len(os.listdir(flags.artifact_output_directory)) > 0
        ):
            recorder.emit_warning_message(
                f"Your output directory already exists and is not empty. This will become a fatal error soon.\nUse --timestamp-artifacts to create new subdirectories for each run, and use `fx test --prev artifact-path` to get the path from the previous run.\nDirectory is: {flags.artifact_output_directory}"
            )

        recorder.emit_artifact_directory_path(
            os.path.abspath(flags.artifact_output_directory)
            if flags.artifact_output_directory
            else None
        )

        if (
            flags.artifact_output_directory
            and not flags.timestamp_artifacts
            and not set(sys.argv).intersection(
                set(["--timestamp-artifacts", "--no-timestamp-artifacts"])
            )
        ):
            recorder.emit_warning_message(
                "You have not specified --[no-]timestamp-artifacts.\nArtifact output will overwrite previous runs.\nThe default will soon change to support timestamped directories."
            )
            recorder.emit_instruction_message(
                "Specify --timestamp-artifacts to output in timestamped subdirectories. This will soon become the default."
            )

        # Configure file logging based on flags.
        if flags.log and exec_env.log_file:
            output_file: typing.TextIO
            if exec_env.log_to_stdout():
                output_file = sys.stdout
            else:
                output_file = gzip.open(exec_env.log_file, "wt")
            self._tasks.append(
                asyncio.create_task(log.writer(recorder, output_file))
            )

        # Load the list of tests to execute.
        try:
            tests = await self._load_test_list()
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
                exec_env,
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
        except RuntimeError as e:
            recorder.emit_end(
                "There was an internal error calling the selection matcher program: {e}"
            )
            return 1

        # Check that the selected tests are valid.
        try:
            await self._validate_test_selections(selections)
        except self._SelectionValidationError as e:
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
        if flags.build and not await self._do_build(selections):
            recorder.emit_end("Failed to build.")
            return 1

        if flags.updateifinbase and self._has_tests_in_base(selections):
            status_suffix = (
                "\nStatus output suspended." if termout.is_init() else ""
            )
            recorder.emit_info_message(
                f"\nBuilding update package.{status_suffix}"
            )
            recorder.emit_instruction_message(
                "Use --no-updateifinbase to skip updating base packages."
            )
            build_return_code = await run_build_with_suspended_output(
                exec_env,
                ["build/images/updates"],
                show_output=not exec_env.log_to_stdout(),
            )
            if build_return_code != 0:
                recorder.emit_end(
                    f"Failed to build update package ({build_return_code})"
                )
                return 1
            recorder.emit_info_message(
                "\nRunning an OTA before executing tests"
            )
            ota_result = await execution.run_command(
                *exec_env.fx_cmd_line("ota", "--no-build"),
                recorder=recorder,
                print_verbatim=True,
            )
            if ota_result is None or ota_result.return_code != 0:
                recorder.emit_warning_message(
                    "OTA failed, attempting to run tests anyway"
                )

        # Generate a new test-list.json file based on the built tests.
        try:
            test_list_entries = await self._generate_test_list()
        except (ValueError, RuntimeError) as e:
            recorder.emit_end(
                f"Failed to generate and load test-list.json: {e}"
            )
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
            await self._enumerate_test_cases(selections)
            recorder.emit_end()
            return 0

        # From this point on, separately handle exit requests so that tests can
        # close cleanly.
        _immediate_exit_task.cancel()
        if self._end_execution_request_event.is_set():
            # We raced with an exit request.
            # Request termination and await exit.
            self._termination_callback_event.set()
            await asyncio.sleep(3600)

        # Finally, run all selected tests.
        if not await self._run_all_tests(selections):
            if not flags.has_debugger() and not flags.host:
                # Note: it is important that we put --break-on-failure before the rest of the command
                # line arguments so that it is ensured that this fx test argument comes before any extra
                # arguments are passed through to the test executable (e.g. after "--").
                recorder.emit_instruction_message(
                    "\nTo debug with zxdb: fx test --break-on-failure {}\n".format(
                        " ".join(sys.argv[1:])
                    )
                )

            recorder.emit_end("Failed to run all tests")

            return 1

        recorder.emit_end()
        return 0

    async def _load_test_list(
        self,
    ) -> list[test_list_file.Test]:
        """Load the input files listing tests and parse them into a list of Tests.

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

        recorder = self._recorder
        exec_env = self._exec_env
        assert exec_env is not None

        # Load the tests.json file.
        parse_id: event.Id | None = None
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
        except (
            tests_json_file.TestFileError,
            json.JSONDecodeError,
            IOError,
        ) as e:
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

    async def _generate_test_list(
        self,
    ) -> dict[str, test_list_file.TestListEntry]:
        recorder = self._recorder
        exec_env = self._exec_env
        assert exec_env is not None

        with tempfile.TemporaryDirectory() as td:
            out_path = os.path.join(td, "test-list.json")
            result = await execution.run_command(
                *exec_env.fx_cmd_line(
                    "test_list_tool",
                    "--build-dir",
                    exec_env.out_dir,
                    "--input",
                    exec_env.test_json_file,
                    "--output",
                    out_path,
                    "--test-components",
                    os.path.join(exec_env.out_dir, "test_components.json"),
                    "--ignore-device-test-errors",
                ),
                recorder=recorder,
            )
            if result is None or result.return_code != 0:
                suffix = ""
                if result is not None:
                    suffix = f":\n{result.stdout}\n{result.stderr}"
                raise RuntimeError(
                    f"Could not generate a new test-list.json{suffix}"
                )

            exec_env.test_list_file = out_path

            # Load the generated test-list.json file.
            try:
                parse_id = recorder.emit_start_file_parsing(
                    exec_env.relative_to_root(exec_env.test_list_file),
                    exec_env.test_list_file,
                )
                test_list_entries = (
                    test_list_file.TestListFile.entries_from_file(
                        exec_env.test_list_file
                    )
                )
                recorder.emit_end(id=parse_id)
                return test_list_entries
            except (
                dataparse.DataParseError,
                json.JSONDecodeError,
                IOError,
            ) as e:
                raise e

    class _SelectionValidationError(Exception):
        """A problem occurred when validating test selections.

        The message contains a human-readable explanation of the problem.
        """

    async def _validate_test_selections(
        self,
        selections: selection_types.TestSelections,
    ) -> None:
        """Validate the selections matched from tests.json.

        Args:
            selections (TestSelections): The selection output to validate.

        Raises:
            SelectionValidationError: If the selections are invalid.
        """

        recorder = self._recorder
        flags = self._flags
        exec_env = self._exec_env
        assert exec_env is not None

        missing_groups: list[selection_types.MatchGroup] = []

        for group, matches in selections.group_matches:
            if not matches:
                missing_groups.append(group)

        if missing_groups:
            recorder.emit_warning_message(
                "\nCould not find any tests to run for at least one set of arguments you provided."
            )
            missing_group_with_name = next(
                filter(lambda x: len(x.names) > 0, missing_groups), None
            )
            if flags.exact and missing_group_with_name is not None:
                recorder.emit_instruction_message(
                    f" --exact does not match packages or components by default\n Did you mean: --exact --package {missing_group_with_name}?"
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
                    suggestion_args = [
                        "search-tests",
                        f"--max-results={flags.suggestion_count}",
                        "--color" if flags.style else "--no-color",
                        arg,
                    ]
                    if threshold is not None:
                        suggestion_args += ["--threshold", str(threshold)]
                    return exec_env.fx_cmd_line(*suggestion_args)

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
                    recorder.emit_verbatim_message(
                        f"\nFor `{group}`, did you mean any of the following?\n"
                    )
                    recorder.emit_verbatim_message(output.stdout)

        if missing_groups:
            raise self._SelectionValidationError(
                "No tests found for the following selections:\n "
                + "\n ".join([str(m) for m in missing_groups])
            )

    async def _do_build(
        self,
        tests: selection_types.TestSelections,
    ) -> bool:
        """Attempt to build the selected tests.

        Args:
            tests (selection.TestSelections): Tests to attempt to build.

        Returns:
            bool: True only if the tests were built and published, False otherwise.
        """
        recorder = self._recorder
        exec_env = self._exec_env
        assert exec_env is not None
        allow_build_updates = self._flags.build_updates

        # Labels start with // and end with a toolchain, starting with
        # '('. Both toolchain and '//' need to be omitted for building
        # device tests through fx build.
        label_to_rule = re.compile(r"//([^()]+)\((.+)\)")
        build_targets_by_toolchain: defaultdict[str, list[str]] = defaultdict(
            list
        )
        for selection in tests.selected:
            label = (
                selection.build.test.package_label or selection.build.test.label
            )
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

        if tests.has_e2e_test() and allow_build_updates:
            build_command_line.extend(["--default", "updates"])
            recorder.emit_instruction_message(
                "E2E test selected, building updates package"
            )

        build_id = recorder.emit_build_start(targets=build_command_line)
        recorder.emit_instruction_message("Use --no-build to skip building")

        status_suffix = " Status output suspended." if termout.is_init() else ""
        recorder.emit_info_message(f"\nExecuting build.{status_suffix}")

        await asyncio.sleep(0.1)

        return_code = await run_build_with_suspended_output(
            exec_env,
            build_command_line,
            show_output=not exec_env.log_to_stdout(),
        )

        error = None
        if return_code != 0:
            error = f"Build returned non-zero exit code {return_code}"
        if error is not None:
            recorder.emit_end(error, id=build_id)
            return False

        if tests.has_device_test():
            try:
                await self._publish_packages(build_id)
            except self._PublishException as e:
                error = e.reason

        if not error and not await self._post_build_checklist(tests, build_id):
            error = "Post build checklist failed"

        recorder.emit_end(error, id=build_id)

        return error is None

    @dataclass
    class _PublishException(Exception):
        """Exception that is raised if we fail to publish packages."""

        reason: str

    async def _publish_packages(self, build_id: event.Id) -> None:
        """Publish packages that were just built.

        Args:
            build_id (event.Id): The event of the parent build to nest events under.

        Raises:
            self._PublishException: If publishing fails for any reason.
        """
        recorder = self._recorder
        exec_env = self._exec_env
        assert exec_env is not None

        amber_directory = os.path.join(exec_env.out_dir, "amber-files")
        delivery_blob_type = self._read_delivery_blob_type()

        # This manifest file is updated only following the build
        # process, which poses a problem when we want to use `fx add-test`
        # and then expect the test to work without a full
        # rebuild. To solve this problem, we actually synthesize a
        # new package manifest consisting of the original
        # all_package_manifests.list plus any new packages listed in
        # the generated file "package_manifests_from_metadata.list.package_metadata".
        # The new combined file will contain tests added using `fx add-test`.
        all_package_manifests = os.path.join(
            exec_env.out_dir,
            *self._ALL_PACKAGE_MANIFESTS_PATH,
        )

        # Load the original package manifest list.
        package_manifest: dict[str, typing.Any]
        with open(all_package_manifests) as f:
            package_manifest = json.load(f)

        manifest_list: list[str]
        try:
            manifest_list = package_manifest["content"]["manifests"]
        except KeyError:
            raise self._PublishException(
                "BUG: Failed to load manifest list from all_package_manifests.list\nPlease file a bug."
            )

        # Load the generated list file, which just has a package manifest path per line.
        manifests_metadata_path = os.path.abspath(
            os.path.join(
                exec_env.out_dir,
                *self._PACKAGE_MANIFESTS_FROM_METADATA_PATH,
            )
        )

        # Read all lines from the generated file, merging them back into the package manifest.
        if os.path.isfile(manifests_metadata_path):
            lines: list[str]
            with open(manifests_metadata_path) as f:
                lines = [
                    stripped_line
                    for l in f.readlines()
                    if (stripped_line := l.strip()) != ""
                ]
            manifest_list = list(set(manifest_list).union(lines))

        package_manifest["content"]["manifests"] = manifest_list

        with tempfile.TemporaryDirectory() as td:
            # Generate a merged temporary manifest.
            temp_manifest_path = os.path.join(td, "temp_manifest.list")
            with open(temp_manifest_path, "w") as f:
                json.dump(package_manifest, f)

            # Publish the packages listed in the merged manifest.
            publish_args = (
                exec_env.fx_cmd_line(
                    "ffx",
                    "repository",
                    "publish",
                    "--trusted-root",
                    os.path.abspath(
                        os.path.join(amber_directory, "repository/root.json")
                    ),
                    "--ignore-missing-packages",
                    "--time-versioning",
                )
                + (
                    ["--delivery-blob-type", str(delivery_blob_type)]
                    if delivery_blob_type is not None
                    else []
                )
                + [
                    "--package-list",
                    temp_manifest_path,
                    os.path.abspath(amber_directory),
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
                raise self._PublishException("Failure publishing packages.")
            elif output.return_code != 0:
                raise self._PublishException(
                    f"Publish returned non-zero exit code {output.return_code}"
                )

    def _read_delivery_blob_type(
        self,
    ) -> int | None:
        """Read the delivery blob type from the output directory.

        The delivery_blob_config.json file contains a "type" field that must
        be passed along to package publishing if set.

        This functions attempts to load the file and returns the value of that
        field if set.

        Returns:
            int | None: The delivery blob type, if found. None otherwise.
        """
        recorder = self._recorder
        exec_env = self._exec_env
        assert exec_env is not None

        expected_path = os.path.join(
            exec_env.out_dir, "delivery_blob_config.json"
        )
        id = recorder.emit_start_file_parsing(
            "delivery_blob_config.json", expected_path
        )
        if not os.path.isfile(expected_path):
            recorder.emit_end(
                error="Could not find delivery_blob_config.json in output",
                id=id,
            )
            return None

        with open(expected_path) as f:
            val: dict[str, typing.Any] = json.load(f)
            recorder.emit_end(id=id)
            return int(val["type"]) if "type" in val else None

    def _has_tests_in_base(
        self,
        tests: selection_types.TestSelections,
    ) -> bool:
        recorder = self._recorder
        exec_env = self._exec_env
        assert exec_env is not None

        base_file = os.path.join(exec_env.out_dir, "base_packages.list")
        parse_id = recorder.emit_start_file_parsing(
            "base_packages.list", base_file
        )

        manifests: list[str]
        try:
            with open(base_file) as f:
                contents = json.load(f)
            manifests = contents["content"]["manifests"]
        except (json.JSONDecodeError, KeyError) as e:
            recorder.emit_end(f"Parsing file failed: {e}", id=parse_id)
            raise e
        except FileNotFoundError:
            # No base packages found
            recorder.emit_end(id=parse_id)
            return False

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

    async def _post_build_checklist(
        self,
        tests: selection_types.TestSelections,
        build_id: event.Id,
    ) -> bool:
        """Perform a number of post-build checks to ensure we are ready to run tests.

        Args:
            tests (selection.TestSelections): Tests selected to run.
            build_id (event.Id): ID of the build event to use at the parent of any operations executed here.

        Returns:
            bool: True only if post-build checks passed, False otherwise.
        """
        recorder = self._recorder
        exec_env = self._exec_env
        assert exec_env is not None

        if tests.has_device_test() and await has_device_connected(
            exec_env, recorder, parent=build_id
        ):
            try:
                if self._has_tests_in_base(tests):
                    recorder.emit_info_message(
                        "Some selected test(s) are in the base package set. Running an OTA."
                    )
                    output = await execution.run_command(
                        *exec_env.fx_cmd_line("ota"),
                        recorder=recorder,
                        print_verbatim=True,
                    )
                    if not output or output.return_code != 0:
                        recorder.emit_warning_message("OTA failed")
                        return False
            except IOError as e:
                return False

        return True

    async def _run_all_tests(
        self,
        tests: selection_types.TestSelections,
    ) -> bool:
        """Execute all selected tests.

        Args:
            tests (selection.TestSelections): The selected tests to run.

        Returns:
            bool: True only if all tests ran successfully, False otherwise.
        """
        flags = self._flags
        recorder = self._recorder
        exec_env = self._exec_env
        assert exec_env is not None

        max_parallel = flags.parallel
        if tests.has_device_test() and not await has_device_connected(
            exec_env,
            recorder,
        ):
            recorder.emit_warning_message(
                "\nCould not find a running package server."
            )
            recorder.emit_instruction_message(
                "\nYou do not seem to have a package server running, but you have selected at least one device test.\nEnsure that you have `fx serve` running and that you have selected your desired device using `fx set-device`.\n"
            )
            return False

        # This is an error since no tests that were selected involved the device, even if the --host
        # flag was not specified on the command line. If a test selection includes _some_ device tests,
        # those are allowed. The existence of host tests among device tests is not a problem for the
        # debugger integration, but users may be confused if they try to debug host tests with
        # automatic selection.
        if not tests.has_device_test() and flags.has_debugger():
            recorder.emit_warning_message(
                "\n--break-on-failure and --breakpoint flags are not supported with host tests."
            )
            recorder.emit_instruction_message(
                "\nRemove the --break-on-failure and --breakpoint flags to run these host-only tests."
            )
            return False

        device_environment: environment.DeviceEnvironment | None = None
        if tests.has_device_test():
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
                        None if not test.needs_device() else device_environment
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

        async def test_cancellation_handler() -> None:
            await self._end_execution_request_event.wait()
            recorder.emit_warning_message("Received request to terminate...")
            abort_all_tests_event.set()

        test_cancellation_handler_task = asyncio.Task(
            test_cancellation_handler()
        )

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
                status: event.TestSuiteStatus = (
                    event.TestSuiteStatus.FAILED_TO_START
                )
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
                                        abort_signal=abort_all_tests_event,
                                    )
                                ),
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
                    status = event.TestSuiteStatus.FAILED_TO_START
                    message = str(e)
                except execution.TestSkipped as e:
                    status = event.TestSuiteStatus.SKIPPED
                    message = str(e)
                except (execution.TestTimeout, execution.TestFailed) as e:
                    if isinstance(e, execution.TestTimeout):
                        status = event.TestSuiteStatus.TIMEOUT
                        test_failure_observed = True
                    elif self._end_execution_request_event.is_set():
                        # Terminating the tests will end up here, since they will have a
                        # non-zero exit code following SIGTERM.
                        status = event.TestSuiteStatus.ABORTED
                        message = "Test suite aborted due to user interrupt."
                    else:
                        status = event.TestSuiteStatus.FAILED
                        test_failure_observed = True

                    # Abort other tests in this group.
                    to_run.abort_group.set()

                    if flags.fail:
                        # Abort all other running tests, dropping through to the
                        # following run state code to trigger any waiting executors.
                        abort_all_tests_event.set()
                finally:
                    recorder.emit_test_suite_ended(
                        test_suite_id, status, message
                    )

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

        test_cancellation_handler_task.cancel()

        return (
            not test_failure_observed
            and not self._end_execution_request_event.is_set()
        )

    async def _enumerate_test_cases(
        self,
        tests: selection_types.TestSelections,
    ) -> None:
        flags = self._flags
        recorder = self._recorder
        exec_env = self._exec_env
        assert exec_env is not None

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


@functools.lru_cache
async def has_device_connected(
    exec_env: environment.ExecutionEnvironment,
    recorder: event.EventRecorder,
    parent: event.Id | None = None,
) -> bool:
    """Check if a device is connected for running target tests.

    Args:
        recorder (event.EventRecorder): Recorder for events.
        parent (event.Id, optional): Parent task ID. Defaults to None.

    Returns:
        bool: True only if a device is available to run target tests.
    """
    output = await execution.run_command(
        *exec_env.fx_cmd_line(
            "is-package-server-running",
        ),
        recorder=recorder,
        parent=parent,
    )
    return output is not None and output.return_code == 0


async def run_build_with_suspended_output(
    exec_env: environment.ExecutionEnvironment,
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
        exec_env.fx_cmd_line("build", *build_command_line),
        stdout=stdout,
        stderr=stderr,
    )
    return return_code


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
