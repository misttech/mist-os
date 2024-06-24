# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import threading
import subprocess

from typing import List, Callable, IO


Handler = Callable[[bytes], None]


def handle_stream(stream: IO[bytes], output_handler: Handler) -> None:
    """Calls `output_handler` with the output bytes. Makes it easy to unit test `CommandRunner`.

    Args:
        stream (IO[bytes]): Stream of output strings
        output_handler (Handler): Output Handler
    """
    for line in stream:
        output_handler(line)
    stream.close()


def run_command(
    cmd: List[str],
    output_handler: Handler,
    error_handler: Handler,
) -> int:
    """Execute command and collect output

    Args:
        cmd (List[str]): Command to execute
        output_handler (Handler): Handler for stdout
        error_handler (Handler): Handler for stderr

    Returns:
        int: Exit code of the process
    """
    process = subprocess.Popen(
        cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE
    )
    stdout_thread = threading.Thread(
        target=handle_stream, args=(process.stdout, output_handler)
    )
    stderr_thread = threading.Thread(
        target=handle_stream, args=(process.stderr, error_handler)
    )

    stdout_thread.start()
    stderr_thread.start()

    process.wait()

    # Wait for the threads to finish
    stdout_thread.join()
    stderr_thread.join()

    return process.returncode
