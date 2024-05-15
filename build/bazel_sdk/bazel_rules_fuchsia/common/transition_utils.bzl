# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Common utilities for transition functions."""

def set_command_line_option_value(input_args, option_prefix, option_value):
    """Set or reset of command-line option such as "--foo=bar".

    Args:
        input_args: [list(string)] A input list of command arguments.
        option_prefix: [string] The option prefix (e.g. "--foo=")
        option_value: [string] The new option value (e.g. "bar").
    Returns:
        If the option already appears in the input, change its value to option_value

        If multiple instances of the option appear in the input, they are all
        changed to the new value.

        Otherwise (i.e. if the option prefix is not in the input), return the input list
        with "${option_prefix}${option_value}" appended to it.
    """
    new_arg = option_prefix + option_value
    result = []
    replaced = False
    for arg in input_args:
        if arg.startswith(option_prefix):
            arg = new_arg
            replaced = True
        result.append(arg)

    if not replaced:
        result.append(new_arg)

    return result

def remove_command_line_option_values(input_args, option_prefix):
    """Remove all instances of a command-line option from an input arguments list.

    Args:
        input_args: [list(string)] A input list of command arguments.
        option_prefix: [string] The option prefix (e.g. "--foo=").
    Returns:
        new argument list, with all argument beginning with option_prefix removed.
    """
    return [arg for arg in input_args if not arg.startswith(option_prefix)]
