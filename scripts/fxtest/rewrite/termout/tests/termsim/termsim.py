# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import io
import typing


class Terminal:
    """Terminal simulator for testing.

    This class can be set as stdout to simulate a terminal. It implements only
    the bare minimum ANSI escape codes needed to test termout, as follows:
    - Style commands; ignored.
    - Cursor up.
    - Clear line, valid only after CR.
    - Next line, valid only as \\n.

    Attributes:
        lines (List[str]): The list of lines that would show on a terminal.
    """

    def __init__(self, width: int):
        self._width = width
        self.lines: typing.List[str] = [""]
        self._line_position: int = 0
        self._current_write = io.StringIO()

    def write(self, value: str) -> int:
        """Implementation of io write.

        Args:
            value (str): Value to write.

        Returns:
            int: Number of bytes written.
        """
        return self._current_write.write(value)

    def flush(self):
        """Implementation of io flush.

        To ensure that the library flushes appropriately, the lines field
        is updated only on flush.
        """
        expecting_clear = False

        chars = [ch for ch in self._current_write.getvalue()]
        chars.reverse()
        while chars:
            ch = chars.pop()
            if ch == "\n":
                self._line_position += 1
                while len(self.lines) <= self._line_position:
                    self.lines.append("")
            elif ch == "\r":
                expecting_clear = True
            elif ch == "\x1B":
                # Handle escape sequences.
                next_ch = chars.pop()
                code = ""
                while next_ch and not next_ch.isalpha():
                    code += next_ch
                    next_ch = chars.pop()
                if next_ch:
                    code += next_ch
                if not code:
                    raise TerminalError("Empty escape code")
                if code[-1] == "m":
                    # Color style, skip
                    pass
                elif code == "[1A":
                    # Go up a line
                    self._line_position -= 1
                elif code == "[2K":
                    # Clear the line
                    if not expecting_clear:
                        raise TerminalError(
                            "Cleared a line without going back to the beginning"
                        )
                    expecting_clear = False
                    self.lines[self._line_position] = ""
                else:
                    raise TerminalError(f"Unknown escape code \\x1B{code}")
            else:
                if len(self.lines[self._line_position]) >= self._width:
                    # Simulate a newline to go to the next line.
                    chars.append(ch)
                    chars.append("\n")
                else:
                    self.lines[self._line_position] += ch

        if expecting_clear:
            raise TerminalError("Expected a clear line sequence after CR")

        self._current_write = io.StringIO()


class TerminalError(Exception):
    """There was an error simulating a terminal."""
