# Tools for Power

This directory contains some engineering tools for power analysis.

## `fx power-digest`

This prototype tool takes Power Broker's Inspect data, along with
that from System Activity Governor and Fuchsia Suspend HAL,
and renders them in a denser format for human consumption.

### Written in Python

For easier text processing, this prototype tool has been written
in Python. Expect it to change frequently until it settles. A form
of this tool may be reimplemented as a production quality tool, with
tests etc, in FFX or FSV in future.

### BUILD structure

To keep ownership clear, the Python file itself is placed in
`src/power/tools`. However, some helper files like `tools/devshell/BUILD.gn`
`tools/devshell/contrib/power-digest` provide the structure to run it
from the command line as `fx power-digest`.

### Standalone

Certain users without access to the source tree may still run the script by
first obtaining a copy of it, and running it like so:

```
$ python3 power_digest.py
```

### Testing scenarios

Quick rundown of scenarios.

#### Ingress variants

1. Pull snapshot from device: `fx power-digest`
1. Use snapshot from file: `fx power-digest snapshot.zip`
1. Use bugreport from file: `fx power-digest bugreport.zip`
1. Use inspect.json file: `fx power-digest inspect.json`

Expected: output to terminal

#### Egress variants

1. Specify output file: `fx power-digest inspect.json -o digest.txt`
1. Specify output file: `fx power-digest inspect.json --out digest.txt`
1. Specify CSV output: `fx power-digest --csv inspect.json -o csv.txt`

### Alternate invocations

1. `python3 power_digest.py inspect.json`
