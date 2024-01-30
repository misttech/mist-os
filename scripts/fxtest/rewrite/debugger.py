# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import typing
import test_list_file

import atexit
import os
import string
import signal
import subprocess
import sys
import random
import tempfile


def spawn(tests: typing.List[test_list_file.Test]) -> subprocess.Popen:
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
        "--stream-file",
        fifo,
        # TODO(https://fxbug.dev/322225894): This shouldn't be needed, but rust tests install a
        # non-abort panic hook, which doesn't raise an exception that the debugger can catch. This
        # breakpoint will ensure that the test stops if the panic hook is run. Regular rust binaries
        # do not have this issue, so this typically isn't needed when debugging non-tests. For C++
        # tests this will appear as a breakpoint that never resolves to any symbol.
        "--execute",
        "break std::panicking::default_hook",
    ]

    # Use start_new_session, rather than just using os.setpgrp as a preexec function. This enables
    # the subprocess to also control the tty, which zxdb requires, as well as terminating the entire
    # process group when all the tests have finished.
    debugger_process = subprocess.Popen(
        args=zxdb_args, start_new_session=True, stderr=subprocess.STDOUT
    )

    def _cleanup():
        os.remove(fifo)
        # The debugger should be terminated when all of the test suites have run. If not, forcefully
        # kill it now.
        if debugger_process.returncode is None:
            pg = os.getpgid(debugger_process.pid)
            os.killpg(pg, signal.SIGKILL)

    atexit.register(_cleanup)

    # Replace stdout with the named pipe we created.
    sys.stdout = open(fifo, "w")

    return debugger_process
