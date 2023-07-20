#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
A utility class for python scripts writing to terminal output.
"""

import os
import sys


class Terminal:
    suppress = False

    def bold(text: str) -> str:
        return Terminal._style(text, 1)

    def red(text: str) -> str:
        return Terminal._style(text, 91)

    def green(text: str) -> str:
        return Terminal._style(text, 92)

    def yellow(text: str) -> str:
        return Terminal._style(text, 93)

    def supports_color() -> bool:
        return sys.stdout.isatty() and sys.stderr.isatty(
        ) and not os.environ.get('NO_COLOR')

    def _print(*args, **kwargs) -> None:
        if not Terminal.suppress:
            print(*args, **kwargs)

    def fatal(text: str) -> int:
        Terminal._print(
            Terminal.red(Terminal.bold('FATAL: ')) + text, file=sys.stderr)
        sys.exit(1)

    def error(text: str) -> None:
        Terminal._print(
            Terminal.red(Terminal.bold('ERROR: ')) + text, file=sys.stderr)

    def warn(text: str) -> None:
        Terminal._print(
            Terminal.yellow(Terminal.bold('WARNING: ')) + text, file=sys.stderr)

    def info(text: str) -> None:
        Terminal._print(Terminal.green(Terminal.bold('INFO: ')) + text)

    def _style(text: str, escape_code: int) -> str:
        if Terminal.supports_color():
            return f'\033[{escape_code}m{text}\033[0m'
        else:
            # If neither stdout nor stderr is not a tty then any styles likely
            # won't get rendered correctly when the text is eventually printed,
            # so don't apply the style.
            return text
