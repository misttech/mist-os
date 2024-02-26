# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import atexit
import os
import random
import string
import subprocess
import sys
import tempfile
import typing

import test_list_file


def spawn(
    tests: typing.List[test_list_file.Test],
    break_on_failure: bool = False,
    breakpoints: typing.List[str] = [],
) -> subprocess.Popen:
    """Spawn zxdb in a subprocess.

    Spawn zxdb and attach to |tests|, while waiting for a test failure reported by either
    exception or software breakpoint. Standard output for this program is redirected to a fifo,
    which zxdb will stream to the console. The debugger is spawned in a synchronous process since
    zxdb will be handling all of the stdio streams itself and will take control of forwarding IO
    from python back to the console. The caller has no responsibility to deal with any input or
    output from the spawned process.

    Args:
        tests (typing.List[test_list_file.Test]): List of tests selected to be executed.

    Returns:
        subprocess.Popen: process handle for the zxdb process group.

        Note: The caller is responsible for killing the process group associated with the returned
        process.
    """
    fifo = os.path.join(
        tempfile.gettempdir()
        + "/zxdbpipe-"
        + "".join(
            random.choice(string.ascii_letters + string.digits)
            for _ in range(6)
        )
    )

    os.mkfifo(fifo)

    attach_args = []
    for test in tests:
        attach_args.extend(["--attach", test.info.name])

    # If only --breakpoint was specified on the command line (we won't get here if neither
    # debug option was specified), we want to output a more general message than "test
    # failure". Zxdb will default to filling in the type of exception in this slot if it's
    # unspecified, so we don't need to specify any additional text. If both options are
    # specified, it is impossible to know which one will happen first, so use the more specific
    # text.
    embedded_mode_context_args = []
    if break_on_failure:
        embedded_mode_context_args = ["--embedded-mode-context", "test failure"]

    zxdb_args = [
        "fx",
        "ffx",
        "debug",
        "connect",
        "--new-agent",
        *attach_args,
        "--",
        "--console-mode",
        "embedded",
        *embedded_mode_context_args,
        "--stream-file",
        fifo,
    ]

    # TODO(https://fxbug.dev/322225894): This shouldn't be needed, but rust tests install a
    # non-abort panic hook, which doesn't raise an exception that the debugger can catch. This
    # breakpoint will ensure that the test stops if the panic hook is run. Regular rust binaries
    # do not have this issue, so this typically isn't needed when debugging non-tests. For C++
    # tests this will appear as a breakpoint that never resolves to any symbol.
    if break_on_failure:
        zxdb_args += [
            "--execute",
            "break std::panicking::default_hook",
        ]

    # Add the requested breakpoints.
    for bp in breakpoints:
        zxdb_args += ["--execute", f"break {bp}"]

    # Use start_new_session, rather than just using os.setpgrp as a preexec function. This enables
    # the subprocess to also control the tty, which zxdb requires, as well as terminating the entire
    # process group when all the tests have finished.
    debugger_process = subprocess.Popen(
        args=zxdb_args, start_new_session=True, stderr=subprocess.STDOUT
    )

    def _cleanup():
        # Close stdout. This may have already been done at the end of all the tests in main.py, but
        # we do it again here to catch the ctrl+c case and still try to cleanly restore the terminal
        # and clean up the socket to DebugAgent.
        sys.stdout.close()
        sys.stdout = open(os.devnull, "w")

        os.remove(fifo)

        try:
            # Give zxdb a chance to gracefully shutdown, in the normal case this should return
            # immediately, but when handling ctrl+c in embedded mode we wait some time to run
            # cleanup routines.
            debugger_process.wait(5)
        except subprocess.TimeoutExpired as e:
            sys.stderr.write(f"{e}\n")
            sys.stderr.flush()
        finally:
            # zxdb should have gracefully exited by now, if it hasn't forcefully terminate and
            # inform the user that they may need to reset their terminal.
            if debugger_process.poll() is None:
                sys.stderr.write(
                    "⚠️  Warning: zxdb did not exit normally. `reset` will fix your terminal ⚠️\n"
                )
                sys.stderr.flush()
                debugger_process.terminate()

    atexit.register(_cleanup)

    # Replace stdout with the named pipe we created and enable line buffering.
    # Note: 1 == line buffered. See https://docs.python.org/3/library/functions.html#open.
    sys.stdout = open(fifo, "w", buffering=1)

    return debugger_process
