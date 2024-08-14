# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import itertools
import json
import os
import subprocess
import sys
import zipfile
from collections import deque
from io import BufferedReader, StringIO, TextIOWrapper
from tempfile import TemporaryDirectory
from typing import Any


# Obtain inspect.json, try a few ways.
def get_inspect_json(file: BufferedReader) -> str:
    if not zipfile.is_zipfile(file):
        # Maybe the file is just inspect.json
        print(f"Not an archive, assuming JSON file for {file}", file=sys.stderr)
        with TextIOWrapper(file, encoding="UTF-8") as f:
            f.seek(0, 0)
            return f.read()
    with zipfile.ZipFile(file) as archive:
        try:
            with archive.open("inspect.json") as json_file:
                return json_file.read().decode()
        except KeyError:
            print(
                f"Did not find inspect.json in {file}, trying another format.",
                file=sys.stderr,
            )
        with archive.open("dumpstate_board.bin") as snapshot_file:
            with zipfile.ZipFile(snapshot_file) as f:
                with f.open("inspect.json") as json_file:
                    return json_file.read().decode()


def find_dict(moniker: str, json: list[tuple[Any, Any]]) -> dict[Any, Any]:
    queue = deque(json)
    while queue:
        curr = queue.popleft()
        if isinstance(curr, dict) and curr.get("moniker") == moniker:
            return curr
        if isinstance(curr, str):
            continue
        for child in curr:
            queue.append(child)
    return {}


def key_within(content: dict[Any, Any], key: str, start: int, end: int) -> bool:
    return key in content and start < content[key] and content[key] < end


def main() -> int:
    # arg setup
    parser = argparse.ArgumentParser(
        "Create compact summary of Power Broker Inspect events from snapshot"
    )
    parser.add_argument(
        "input",
        nargs="?",
        type=argparse.FileType("rb"),
        help="A snapshot.zip or inspect.json file. If not set, will try `ffx target snapshot`.",
    )
    parser.add_argument(
        "-o",
        "--output",
        type=argparse.FileType("w"),
        default=sys.stdout,
        help="The output file, defaults to stdout.",
    )
    parser.add_argument("--csv", action=argparse.BooleanOptionalAction)
    args = parser.parse_args()

    # if needed, get snapshot
    if not args.input:
        print("Reading new snapshot ... ", file=sys.stderr)
        tempdir = TemporaryDirectory()
        command = f"ffx target snapshot -d {tempdir.name}"
        report = subprocess.check_call(
            command.split(), stdout=sys.stderr, stderr=sys.stderr
        )
        args.input = open(os.path.join(tempdir.name, "snapshot.zip"), "rb")

    # open JSON file in snapshot
    json_string = get_inspect_json(args.input)

    # convert JSON into py dict
    json_contents = []
    try:
        json_contents.append((args.input.name, json.loads(json_string)))
    except json.JSONDecodeError as err:
        print(
            f"Failed to parse JSON from {args.input.name}: {err}",
            file=sys.stderr,
        )
        return 1

    # extract power topology and events
    pb = find_dict(moniker="bootstrap/power-broker", json=json_contents)
    graph = pb["payload"]["root"]["broker"]["topology"]["fuchsia.inspect.Graph"]
    topology = graph["topology"]
    events = graph["events"]

    event_keys_sorted = sorted(events)
    start_idx = int(event_keys_sorted[0])
    end_idx = int(event_keys_sorted[-1])

    # one line per event
    lines = []
    level_durations = {}
    lease_durations = {}
    for n in sorted(events):  # list of keys in order
        # each line:  sequence #, time, event name, what, entity, duration
        curr = events[n]
        line = []  # join line parts later.
        line.append(f"#{n}")
        when = curr["@time"] / 1_000_000_000
        line.append(f"{when:.{9}f}")  # RHS of dot is milli/micro/nano seconds
        if curr["event"] == "update_key" and curr["key"] == "required_level":
            # level demand
            line.append(f"required_level")
            line.append(f'{curr["update"]}')
            line.append(f'{curr["vertex_id"]}')
        elif curr["event"] == "update_key" and curr["key"] == "current_level":
            # level comply
            line.append("current_level")
            line.append(f'{curr["update"]}')
            line.append(f'{curr["vertex_id"]}')
            # reverse search for matching event
            for r in range(int(n) - 1, start_idx - 1, -1):
                prev = events[str(r)]
                if (
                    prev.get("key") == "required_level"
                    and prev["vertex_id"] == curr["vertex_id"]
                ):
                    dur = (curr["@time"] - prev["@time"]) / 1000  # microseconds
                    line.append(f"{dur}")
                    level_durations[n] = (curr["vertex_id"], dur)
                    break
        elif curr["event"] == "update_key" and curr["key"].startswith(
            "lease_status_"
        ):
            if curr["update"] == "Pending":
                # lease lifecycle
                line.append(f"lease_status")
                line.append("Pend")
                line.append(f'{curr["vertex_id"]}')
                # reverse search for matching event
                for r in range(int(n) - 1, start_idx - 1, -1):
                    prev = events[str(r)]
                    if prev.get("key") == curr["key"]:
                        dur = (
                            curr["@time"] - prev["@time"]
                        ) / 1000  # microseconds
                        line.append(f"{dur}")
                        lease_durations[n] = (
                            curr["vertex_id"],
                            dur,
                        )
            elif curr["update"] == "Satisfied":
                # lease lifecycle
                line.append("lease_status")
                line.append("Satf")
                line.append(f'{curr["vertex_id"]}')
                # reverse search for matching event
                for r in range(int(n) - 1, start_idx - 1, -1):
                    prev = events[str(r)]
                    if prev.get("key") == curr["key"]:
                        dur = (
                            curr["@time"] - prev["@time"]
                        ) / 1000  # microseconds
                        line.append(f"{dur}")
                        lease_durations[n] = (
                            curr["vertex_id"],
                            dur,
                        )
        elif curr["event"] == "update_key" and curr["key"].startswith("lease_"):
            # lease lifecycle, start
            line.append("lease")
            line.append("New")
            line.append(f'{curr["vertex_id"]}')
        elif curr["event"] == "drop_key" and curr["key"].startswith("lease_"):
            # lease lifecycle, end
            line.append("lease")
            line.append("Drop")
            line.append(f'{curr["vertex_id"]}')
        elif curr["event"] == "add_vertex":
            # topology change, new node
            line.append("element")
            line.append("Add")
            line.append(f'{curr["vertex_id"]}')
        elif curr["event"] == "add_edge":
            # topology change, new edge
            line.append("dependency")
            line.append(f'{curr["meta"]}')
            line.append(f'{curr["from"]}->{curr["to"]}')
        elif curr["event"] == "update_key" and curr["key"].isnumeric():
            # topology change, edge update
            line.append("dependency")
            line.append(f"update")
            line.append(f"TBD")
        elif curr["event"] == "remove_vertex" or curr["event"] == "remove_edge":
            # topology change, remove edge
            line.append("dependency")
            line.append(f"update")
            line.append(f"TBD")

        lines.append(line)  # list of list of strings

    # splice in SAG and FSH events too

    # extract system activity governor's suspend/resume events
    sag = find_dict(
        moniker="bootstrap/system-activity-governor", json=json_contents
    )
    sag_events = sag["payload"]["root"]["suspend_events"]

    # extract fuchsia suspend hal's suspend/resume events
    fsh = find_dict(
        moniker="bootstrap/boot-drivers:dev.sys.platform.pt.suspend",
        json=json_contents,
    )
    fsh_events = fsh["payload"]["root"]["suspend_events"]

    # add sag and fsh events into 'lines' and re-sort. exclude if outside PB's event range.
    extra_lines = []
    history_start = events[str(start_idx)]["@time"]
    history_end = events[str(end_idx)]["@time"]
    for e in sag_events.values():
        if key_within(e, "suspended", history_start, history_end):
            time = f'{e["suspended"]/1_000_000_000:.{9}f}'
            extra_lines.append(["sag", time, "suspend->fsh", "_", "sag"])
        elif key_within(e, "resumed", history_start, history_end):
            time = f'{e["resumed"]/1_000_000_000:.{9}f}'
            extra_lines.append(["sag", time, "resume<-fsh", "_", "sag"])
    for e in fsh_events.values():
        if key_within(e, "suspended", history_start, history_end):
            time = f'{e["suspended"]/1_000_000_000:.{9}f}'
            extra_lines.append(["fsh", time, "suspend->zx", "_", "fsh"])
        elif key_within(e, "resumed", history_start, history_end):
            time = f'{e["resumed"]/1_000_000_000:.{9}f}'
            extra_lines.append(["fsh", time, "resume<-zx", "_", "fsh"])
    lines.extend(extra_lines)
    lines = sorted(lines, key=lambda line: float(line[1]))

    if args.csv:
        for line in lines:
            args.output.write(",".join(line) + "\n")
        return 0

    history_duration = float(lines[-1][1]) - float(lines[0][1])
    args.output.write(
        f"Event history, observation period {history_duration:.0f} seconds\n"
    )

    # find formatting alignment
    lines = [
        ["pb seq", "when (sec)", "event", "what", "entity", "took (us)"],
        *lines,
    ]  # include header
    consume_iter = [iter(line) for line in lines]
    zipped = itertools.zip_longest(*consume_iter, fillvalue="")
    mx = list(map(lambda ss: max(len(s) for s in ss), zipped))

    # print with nice formatting
    for line in lines:
        form = (
            f"{line[0]:>{mx[0]}} {line[1]:>{mx[1]}}  "
            f"{line[2]:>{mx[2]}} {line[3]:<{mx[3]}}  {line[4]:<{mx[4]}} "
            f'{line[5] if len(line) > 5 else "":>{mx[5]}}'
        )
        args.output.write(form + "\n")

    # other fun stats too
    args.output.write("\nLevel change durations, Top 10\n")
    top_level_durations = dict(
        sorted(
            level_durations.items(), key=lambda item: item[1][1], reverse=True
        )
    )
    name_max = max(
        len(item[1][0])
        for item in itertools.islice(top_level_durations.items(), 10)
    )
    for key, value in itertools.islice(top_level_durations.items(), 10):
        args.output.write(f"#{key}: {value[0]:<{name_max}} {value[1]} us\n")

    args.output.write("\nLease change durations, Top 10\n")
    top_lease_durations = dict(
        sorted(
            lease_durations.items(), key=lambda item: item[1][1], reverse=True
        )
    )
    name_max = max(
        len(item[1][0])
        for item in itertools.islice(top_lease_durations.items(), 10)
    )
    for key, value in itertools.islice(top_lease_durations.items(), 10):
        args.output.write(f"#{key}: {value[0]:<{name_max}} {value[1]} us\n")

    return 0


if __name__ == "__main__":
    sys.exit(main())
