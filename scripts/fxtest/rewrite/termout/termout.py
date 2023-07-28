# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Terminal output library for overwriting content in a window.

This module contains functionality to show a fixed number of lines
in the user's terminal. The output continually overwrites itself. The
library can be used to write very basic output-only terminal UIs.

Typical Usage:
    if termout.is_valid():
        termout.init()
        termout.write_lines([
          'Hello, world'
        ])
        time.sleep(1)
        termout.write_lines([
          'This is,',
          '  a test...',
        ])
        time.sleep(1)
        termout.write_lines([
            'Goodbye'
        ])
        time.sleep(1)
"""

import atexit
from dataclasses import dataclass
import os
import shutil
import sys
import termios
import threading
import typing

import colorama


class TerminalError(Exception):
    """Raised when there is an exception related to terminal output."""


def is_valid() -> bool:
    """Determine if it is valid to use termout in this environment.

    Returns:
        True if stdout goes somewhere we support (like a TTY), and
        False otherwise.
    """
    return os.isatty(sys.stdout.fileno())


def _cursor_show(show: bool):
    """Show or hide the cursor.

    Args:
        show (bool): Whether to show the cursor or hide it.
    """
    s = f'\033[?25{"h" if show else "l"}'
    sys.stdout.write(s)


def _suspend_echo():
    """Stop echoing to the terminal and hide the cursor.

    Automatically installs a routine to run at process exit to
    restore echoing and cursor visibility.
    """
    fd = sys.stdin.fileno()
    orig_flags = termios.tcgetattr(fd)
    new_flags = termios.tcgetattr(fd)
    new_flags[3] = new_flags[3] & ~termios.ECHO
    termios.tcsetattr(fd, termios.TCSANOW, new_flags)
    _cursor_show(False)

    def cleanup():
        print("\r")
        termios.tcsetattr(fd, termios.TCSANOW, orig_flags)
        _cursor_show(True)

    atexit.register(cleanup)


_init: bool = False


def init():
    """Initialize terminal writing, installing handlers to restore settings at exit.

    Raises:
        TerminalError: stdout is not a valid output for this library.
    """
    global _init
    if _init:
        raise TerminalError("init() may only be called once")
    if not is_valid():
        raise TerminalError(
            "The output stream does not seem to be a valid output. termout must be used on a TTY."
        )
    colorama.init()
    _suspend_echo()
    _init = True


@dataclass
class Size:
    """Represents the width and height of a terminal window."""

    columns: int
    lines: int


def get_size() -> Size:
    """Get the size of the terminal output.

    Returns:
        A Size object containing columns and lines
    """
    size = shutil.get_terminal_size()
    return Size(size.columns, size.lines)


_last_line_count: typing.Optional[int] = None
_write_lock: threading.Lock = threading.Lock()


def reset():
    """Resets the global state of the library.

    termout is stateful, and for testing purposes it is useful to
    reset the internal state.
    """
    global _last_line_count
    with _write_lock:
        _last_line_count = None


def write_lines(
    lines: typing.List[str],
    prepend: typing.Optional[typing.List[str]] = None,
    size: typing.Optional[Size] = None,
):
    """Write a list of lines to the terminal.

    Lines will be truncated if they fail to fit within the current
    terminal window, excluding control characters.

    This function is thread-safe.

    Args:
        lines: The list of lines to write.
        prepend: Optional list of lines to prepend to output.
        size: Optional terminal size override.
    """
    global _last_line_count
    with _write_lock:
        if size is None:
            size = get_size()
        if _last_line_count:
            if len(lines) > _last_line_count:
                # Create extra space
                for _ in range(len(lines) - _last_line_count):
                    print("")
                _last_line_count = len(lines)
            for _ in range(_last_line_count - 1):
                sys.stdout.write(
                    "\r" + colorama.ansi.clear_line() + colorama.ansi.Cursor.UP()
                )
            sys.stdout.write("\r" + colorama.ansi.clear_line())

        for line in prepend or []:
            print(line + colorama.Style.RESET_ALL)

        formatted_lines: typing.List[str] = []

        for line in lines:
            printing = True
            count = 0
            max_index: int = 0
            for index, ch in enumerate(line):
                if ch == "\x1b":
                    printing = False
                elif not printing and ch in ["m", "J", "K", colorama.ansi.BEL]:
                    # Detect the end of an ANSI escape sequence.
                    printing = True
                elif printing:
                    count += 1
                if count > size.columns:
                    break
                max_index = index + 1

            formatted_lines.append(
                "\r"
                + colorama.ansi.clear_line()
                + line[:max_index]
                + colorama.Style.RESET_ALL
            )

        sys.stdout.write("\n".join(formatted_lines))
        sys.stdout.flush()
        _last_line_count = len(lines)
