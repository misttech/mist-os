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
from io import BufferedReader, TextIOWrapper
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


def find_dict_by_moniker(
    moniker: str, json: list[tuple[Any, Any]]
) -> dict[Any, Any]:
    queue = deque(json)
    while queue:
        curr = queue.popleft()
        if isinstance(curr, dict) and moniker in curr.get("moniker"):
            return curr
        if isinstance(curr, str):
            continue
        for child in curr:
            queue.append(child)
    print(f"No inspect found for moniker {moniker}", file=sys.stderr)
    return {}


def find_dict_by_metadata(
    metadata_name: str, json: list[tuple[Any, Any]]
) -> dict[Any, Any]:
    queue = deque(json)
    while queue:
        curr = queue.popleft()
        if (
            isinstance(curr, dict)
            and curr.get("metadata").get("name") == metadata_name
        ):
            return curr
        if isinstance(curr, str):
            continue
        for child in curr:
            queue.append(child)
    print(f"No inspect for metadata.name {metadata_name}", file=sys.stderr)
    return {}


def key_within(content: dict[Any, Any], key: str, start: int, end: int) -> bool:
    return key in content and start < content[key] and content[key] < end


def find_elem_id_for(graph: dict[Any, Any], proper_name: str) -> str:
    for elem in graph:
        if graph[elem]["meta"]["name"] == proper_name:
            return elem
    return f"no-id-for-{proper_name}"


# For each event line in the list, return each element's current level as a map.
# The complete data is a list of maps.
def compute_elem_levels_per_line(lines: list[Any]) -> list[dict[str, int]]:
    collector = []
    levels: dict[str, int] = {}
    for line in lines:
        if line[2] == "current_level":
            levels = levels.copy()  # fresh to mutate
            new_level = int(line[3])
            element = line[4]
            levels[element] = new_level
        elif line[2] == "element" and line[3] == "Drop":
            levels = levels.copy()  # fresh to mutate
            del levels[line[4]]
        # nop lines ref-point to an earlier level map, but to mutate we fork the map.
        collector.append(levels)
    return collector


# For each event line in the list, return a list of same length,
# which describes the correct inbound edges for each (element, level).
# The lifecycle intricacies here are from mapping the Inspect Graph's notion of
# vertices and edges and metadata updates (level->level), to Power Broker concepts.
def compute_deps_per_line(
    lines: list[Any], topology: dict[Any, Any]
) -> list[dict[tuple[str, int], list[tuple[str, int]]]]:
    # for each line, a map: (elem, level) -> [(src1, src1_level), (src2, src2_level)]
    i_maps = []
    i_map: dict[tuple[str, int], list[tuple[str, int]]] = {}
    e_map: dict[str, tuple[str, str]] = {}  # edge_id:n -> (from_elem, to_elem)
    for line in lines:
        if line[2] == "element" and line[3] == "New":
            pass  # nop
        elif line[2] == "element" and line[3] == "Drop":
            elem = line[4]
            i_map = {
                dst: [src for src in src_list if src[0] != elem]
                for dst, src_list in i_map.items()
                if dst[0] != elem
            }  # fresh list value for each key
        elif line[2] == "dependency" and line[3] == "New":
            tokens = line[4].split()
            e_map[tokens[0]] = (tokens[1], tokens[3])
        elif line[2] == "dependency" and line[3] == "Update":
            tokens = line[4].split()
            # e_map lookup can fail due to lossy events prior
            src_elem, dst_elem = "", ""  # avoid None
            if tokens[0] in e_map:
                # usually: we see the earlier lifecycle event
                src_elem, dst_elem = e_map[tokens[0]]
            else:
                # backup: check final topology
                for src in topology:
                    if "relationships" in topology[src]:
                        for dst in topology[src]["relationships"]:
                            edge_id_num = topology[src]["relationships"][dst][
                                "edge_id"
                            ]
                            edge_id_str = f"edge_id:{edge_id_num}"
                            if edge_id_str == tokens[0]:
                                src_elem = src
                                dst_elem = dst
            if not src_elem or not dst_elem:
                print(
                    f"could not compute in-bound deps for line {line} due to missing info.",
                    file=sys.stderr,
                )
                pass  # nop
            src_level = int(tokens[1])
            dst_level = 0
            dst_level_draft = tokens[3]
            is_opportunistic = False
            is_removal = False
            if dst_level_draft == "None":
                is_removal = True
            elif dst_level_draft.endswith("p"):
                is_opportunistic = True  # TODO(jaeheon) store this bit
                dst_level = int(dst_level_draft.rstrip("p"))
            else:
                dst_level = int(dst_level_draft)
            i_map = i_map.copy()  # shallow copy: list values point to old lists
            # remove previous (src_elem,src_level) recorded, inspect key clobber
            for k, v in i_map.items():
                if k[0] == dst_elem:
                    i_map[k] = [s for s in v if s != (src_elem, src_level)]
            if not is_removal:
                key = (dst_elem, dst_level)
                srcs = []
                if key in i_map:
                    srcs = i_map[key].copy()  # don't mutate the past
                srcs.append((src_elem, src_level))
                i_map[key] = srcs
        elif line[2] == "dependency" and line[3] == "Drop":
            print("Encountered an edge drop, unusual.", file=sys.stderr)
            # TODO(jaeheon) e_map lookup can fail if prior events missing
            src_elem, dst_elem = e_map[line[4]]
            # remove all level pairs between src,dst
            i_map = i_map.copy()  # shallow copy: list values point to old lists
            for k, v in i_map.items():
                if k[0] == dst_elem:
                    # fresh list, don't mutate the past
                    i_map[k] = [x for x in v if x[0] != src_elem]
        i_maps.append(i_map)
    return i_maps


def lease_hash_in_list(lease_hash: str, leases: list[tuple[int, str]]) -> bool:
    for _, h in leases:
        if lease_hash == h:
            return True
    return False


# Data structure.
#   For each event in history, we have a map, element->[(level,lease-hash)]
#   that for each element, returns the active leases on that element,
#   as a list of (level,lease-hash) tuples.
#   It would be more precise (and handy) to have (element,level) be the
#   hash key, but the lease Drop lifecycle event does not notate the lease's
#   level, only its element.
# Lifecycle.
#   On "lease_status Satf elem 2 hash": add entry (elem, (2,hash)) to map
#   On "lease Drop elem hash": remove (?,hash) from elem's list value
#      (it may not exist due to event truncation)
#   On "lease_status Pend elem 2 hash": remove (2,hash) from elem's list value
#      (it may not exist due to event truncation)
# Usage.
#   For an element at level 2 that rolls up to a dependency on execution_state,
#   report all (2,hash) entries from the list returned by map[element].
#   These are the leases on (element,2) that prevent the element from occupying
#   a lower power level.
def compute_leases_per_line(
    lines: list[Any],
) -> list[dict[str, list[tuple[int, str]]]]:
    # for each line, a map: elem -> [(level,hash), (level, hash)]
    l_maps = []
    l_map: dict[str, list[tuple[int, str]]] = {}
    for line in lines:
        if line[2] == "lease_status" and line[3] == "Satf":
            tokens = line[4].split()
            elem, level, lease_hash = tokens[0], int(tokens[1]), tokens[2]
            l_map = l_map.copy()  # shallow copy: list values point to old lists
            leases = l_map.setdefault(elem, []).copy()  # don't mutate the past
            leases.append((level, lease_hash))
            l_map[elem] = leases
        elif line[2] == "lease" and line[3] == "Drop":
            tokens = line[4].split()
            elem, lease_hash = tokens[0], tokens[1]
            if lease_hash_in_list(lease_hash, l_map.get(elem, [])):
                l_map = (
                    l_map.copy()
                )  # shallow copy: list values point to old lists
                l_map[elem] = [lh for lh in l_map[elem] if lh[1] != lease_hash]
                if len(l_map[elem]) == 0:
                    del l_map[elem]
        elif line[2] == "lease_status" and line[3] == "Pend":
            tokens = line[4].split()
            elem, level, lease_hash = tokens[0], int(tokens[1]), tokens[2]
            if lease_hash_in_list(lease_hash, l_map.get(elem, [])):
                l_map = (
                    l_map.copy()
                )  # shallow copy: list values point to old lists
                l_map[elem] = [lh for lh in l_map[elem] if lh[1] != lease_hash]
                if len(l_map[elem]) == 0:
                    del l_map[elem]
        l_maps.append(l_map)
    return l_maps


def transitive_active_elements_of_dep(
    in_deps: dict[tuple[str, int], list[tuple[str, int]]],
    levels: dict[str, int],
    dep: tuple[str, int],
) -> list[str]:
    elems: list[str] = []
    queue: deque[tuple[str, int]] = deque()
    queue.append(dep)
    while queue:
        curr = queue.popleft()
        elem = curr[0]
        target_level = curr[1]
        current_level = levels[elem]
        if current_level >= target_level:
            elems.append(elem)
        if curr in in_deps:
            queue.extend(in_deps[curr])
    return elems


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
    pb = find_dict_by_moniker(
        moniker="bootstrap/power-broker", json=json_contents
    )
    graph = pb["payload"]["root"]["broker"]["topology"]["fuchsia.inspect.Graph"]
    events = graph["events"]
    pb_total = len(events)

    events_sorted = sorted(events.items(), key=lambda item: int(item[0]))
    start_idx = int(events_sorted[0][0])
    end_idx = int(events_sorted[-1][0])

    # one line per event
    lines = []
    level_durations = {}
    lease_durations = {}
    for n, _ in events_sorted:  # list of keys in order
        # each line:  sequence #, time, event name, what, entity, duration
        curr = events[n]
        what = curr["event"]
        line = []  # join line parts later.
        line.append(f"#{n}")
        when = curr["@time"] / 1_000_000_000
        line.append(f"{when:.{9}f}")  # RHS of dot is milli/micro/nano seconds
        if what == "update_key" and curr["key"] == "required_level":
            # level demand
            line.append(f"required_level")
            line.append(f'{curr["update"]}')
            line.append(f'{curr["vertex_id"]}')
        elif what == "update_key" and curr["key"] == "current_level":
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
        elif what == "update_key" and curr["key"].startswith("lease_status_"):
            if curr["update"] == "Pending":
                # lease lifecycle
                line.append(f"lease_status")
                line.append("Pend")
                # key's format: lease_status_abcd@level_23
                level_suffix_idx = curr["key"].rindex("@")
                level = curr["key"][level_suffix_idx + len("@level_") :]
                lease_hash = curr["key"][
                    len("lease_status_") : level_suffix_idx
                ][
                    0:6
                ]  # hash truncated to first 6 chars, everywhere
                line.append(f'{curr["vertex_id"]} {level} {lease_hash}')
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
                        break
            elif curr["update"] == "Satisfied":
                # lease lifecycle
                line.append("lease_status")
                line.append("Satf")
                # key's format: lease_status_abcd@level_23
                level_suffix_idx = curr["key"].rindex("@")
                level = curr["key"][level_suffix_idx + len("@level_") :]
                lease_hash = curr["key"][
                    len("lease_status_") : level_suffix_idx
                ][0:6]
                line.append(f'{curr["vertex_id"]} {level} {lease_hash}')
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
                        break
        elif what == "update_key" and curr["key"].startswith("lease_"):
            # lease lifecycle, start
            line.append("lease")
            line.append("New")
            lease_hash = curr["key"][len("lease_") :][0:6]
            line.append(f'{curr["vertex_id"]} {lease_hash}')
        elif what == "drop_key" and curr["key"].startswith("lease_"):
            # lease lifecycle, end
            line.append("lease")
            line.append("Drop")
            lease_hash = curr["key"][len("lease_") :][0:6]
            line.append(f'{curr["vertex_id"]} {lease_hash}')
        elif what == "add_vertex":
            # element lifecycle, start
            line.append("element")
            line.append("New")
            line.append(f'{curr["vertex_id"]}')
        elif what == "remove_vertex":
            # element lifecycle, end
            line.append("element")
            line.append("Drop")
            line.append(f'{curr["vertex_id"]}')
        elif what == "add_edge" and "edge_id" in curr:
            # dependency lifecycle, start
            line.append("dependency")
            line.append(f"New")
            line.append(
                f'edge_id:{curr["edge_id"]} {curr["from"]} -> {curr["to"]}'
            )
        elif what == "update_key" and "edge_id" in curr:
            # dependency lifecycle, modify
            line.append("dependency")
            line.append(f"Update")
            line.append(
                f'edge_id:{curr["edge_id"]} {curr["key"]} -> {curr["update"]}'
            )
        elif what == "drop_key" and "edge_id" in curr:
            # dependency lifecycle, modify
            line.append("dependency")
            line.append(f"Update")
            line.append(f'edge_id:{curr["edge_id"]} {curr["key"]} -> None')
        elif what == "remove_edge" and "edge_id" in curr:
            # dependency lifecycle, end
            line.append("dependency")
            line.append(f"Drop")
            line.append(f'edge_id:{curr["edge_id"]}')

        lines.append(line)  # list of list of strings

    # splice in events from Starnix, SAG, FSH events

    # extract Starnix's suspend/resume events
    starnix = find_dict_by_moniker(
        moniker="core/starnix_runner/kernels:", json=json_contents
    )
    starnix_events = starnix["payload"]["root"]["suspend_events"]
    starnix_total = len(starnix_events)
    starnix_count = 0

    # extract system activity governor's suspend/resume events
    sag = find_dict_by_moniker(
        moniker="bootstrap/system-activity-governor", json=json_contents
    )
    sag_events = sag["payload"]["root"]["suspend_events"]
    sag_total = len(sag_events)
    sag_count = 0

    # extract fuchsia suspend hal's suspend/resume events
    fsh = find_dict_by_metadata(
        metadata_name="generic-suspend",
        json=json_contents,
    )
    fsh_events = fsh["payload"]["root"]["suspend_events"]
    fsh_total = len(fsh_events)
    fsh_count = 0

    # add sag and fsh events into 'lines' and re-sort. exclude if outside PB's event range.
    extra_lines = []
    history_start = events[str(start_idx)]["@time"]
    history_end = events[str(end_idx)]["@time"]

    for e in starnix_events.values():
        if key_within(e, "attempted_at_ns", history_start, history_end):
            time = f'{e["attempted_at_ns"]/1_000_000_000:.{9}f}'
            extra_lines.append(
                ["starnix", time, "suspend->sag", "_", "starnix"]
            )
            starnix_count += 1
        elif key_within(e, "resumed_at_ns", history_start, history_end):
            time = f'{e["resumed_at_ns"]/1_000_000_000:.{9}f}'
            extra_lines.append(["starnix", time, "resume<-sag", "_", "starnix"])
            starnix_count += 1
    for e in sag_events.values():
        if key_within(e, "attempted_at_ns", history_start, history_end):
            time = f'{e["attempted_at_ns"]/1_000_000_000:.{9}f}'
            extra_lines.append(["sag", time, "suspend->fsh", "_", "sag"])
            sag_count += 1
        elif key_within(e, "resumed_at_ns", history_start, history_end):
            time = f'{e["resumed_at_ns"]/1_000_000_000:.{9}f}'
            extra_lines.append(["sag", time, "resume<-fsh", "_", "sag"])
            sag_count += 1
    for e in fsh_events.values():
        if key_within(e, "attempted_at_ns", history_start, history_end):
            time = f'{e["attempted_at_ns"]/1_000_000_000:.{9}f}'
            extra_lines.append(["fsh", time, "suspend->zx", "_", "fsh"])
            fsh_count += 1
        elif key_within(e, "resumed_at_ns", history_start, history_end):
            time = f'{e["resumed_at_ns"]/1_000_000_000:.{9}f}'
            extra_lines.append(["fsh", time, "resume<-zx", "_", "fsh"])
            fsh_count += 1
    lines.extend(extra_lines)
    lines = sorted(lines, key=lambda line: float(line[1]))

    # OUTPUT
    if args.csv:
        for line in lines:
            args.output.write(",".join(line) + "\n")
        return 0

    history_len = float(lines[-1][1]) - float(lines[0][1])
    args.output.write(f"Power events, window of {history_len:.0f} seconds.\n")
    args.output.write(f"PB events: {pb_total} power events form the window.\n")
    args.output.write(
        f"Starnix events: {starnix_count}/{starnix_total} within window.\n"
    )
    args.output.write(f"SAG events: {sag_count}/{sag_total} within window.\n")
    args.output.write(f"FSH events: {fsh_count}/{fsh_total} within window.\n")

    # find formatting alignment
    lines = [
        ["pb seq", "when (sec)", "event", "what", "entity", "took (us)"],
        *lines,
    ]  # include header
    consume_iter = [iter(line) for line in lines]
    zipped = itertools.zip_longest(*consume_iter, fillvalue="")
    mx = list(map(lambda ss: max(len(s) for s in ss), zipped))

    # print with nice formatting
    for i in range(len(lines)):
        line = lines[i]
        form = (
            f"{line[0]:>{mx[0]}} {line[1]:>{mx[1]}}  "
            f"{line[2]:>{mx[2]}} {line[3]:<{mx[3]}}  {line[4]:<{mx[4]}} "
            f'{line[5] if len(line) > 5 else "":>{mx[5]}}\n'
        )
        args.output.write(form)

    # AUXILIARY DATA
    elem_levels_per_line = compute_elem_levels_per_line(lines)
    in_deps_per_line = compute_deps_per_line(lines, graph["topology"])
    leases_per_line = compute_leases_per_line(lines)

    # ANALYSES
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

    # This analysis reports "active" elements through each suspend-resume
    # sequence's minimum non-zero power level period. Specifically, we
    # make use of contemporaneous current level and inbound-dep edges
    # at the time of each event to compute the reason(s) execution_state is
    # non-zero. Intuitively, we grab execution_state at level 2 or 1, and
    # pull out the dependencies, and examine the transitive set of element/level
    # tuples. If an element is "currently" at the level described in the dep,
    # we print it out. The idea is that during a failed suspend sequence,
    # where execution_state did not reach 0, there will be an irreducible set of
    # elements involved in keeping execution_state at 2 or 1.
    line_max = len(lines)  # one past
    suspend_attempts = []  # want tuple list of (suspend-idx, resume-idx)
    suspend_no_matching_resume = 0
    i = 0
    while i < line_max:
        if lines[i][0] == "starnix" and lines[i][2] == "suspend->sag":
            matching_resume = line_max
            for j in range(i + 1, line_max):
                if lines[j][0] == "starnix" and lines[j][2] == "resume<-sag":
                    matching_resume = j
                    break
            if matching_resume == line_max:
                suspend_no_matching_resume += 1
            suspend_attempts.append(
                (i, matching_resume)
            )  # (i, line_max) if no match.
            i = matching_resume  # match: continue search after resume.
            # no match: exit loop
        i += 1
    print(f"\nSuspend attempts: {len(suspend_attempts)}")
    print(
        f"Suspend attempted, but missing resume data: {suspend_no_matching_resume}\n"
    )

    print("Suspends attempts:")
    es = find_elem_id_for(graph["topology"], "execution_state")
    # suspend/resume indices into the lines list
    attempt_count = 0
    for s_idx, r_idx in suspend_attempts:
        print(
            f"Window {attempt_count}: event idx {lines[s_idx + 1][0]}->{lines[r_idx - 1][0]}"
        )
        attempt_count = attempt_count + 1
        if r_idx == line_max:
            print(f"Suspend analysis window")
            print(f"  Duration: {lines[s_idx][1]} -> ...")
            print(f"  Skipping, incomplete data")
            continue  # skip analysis for this suspend round
        if es not in elem_levels_per_line[s_idx]:
            print(f"Suspend analysis window")
            print(f"  Duration: {lines[s_idx][1]} -> {lines[r_idx][1]}")
            print(f"  Skipping, incomplete level data for {es}")
            continue
        # execution_state minimum power level and its index bracket, inclusive
        # assumes suspend->resume has a V-shaped trajectory, a contiguous duration of minimum power
        es_min_level, es_min_idx_a, es_min_idx_b = 255, line_max, line_max
        for idx in range(s_idx, r_idx + 1):
            es_level = elem_levels_per_line[idx][es]
            in_deps = in_deps_per_line[idx]
            if es_level < es_min_level:
                es_min_level = es_level
                es_min_idx_a = idx
            elif es_level == es_min_level:
                es_min_idx_b = idx
        if es_min_level > 0:
            for idx in range(es_min_idx_a, es_min_idx_b + 1):
                levels = elem_levels_per_line[idx]
                in_deps = in_deps_per_line[idx]
                elems_involved = transitive_active_elements_of_dep(
                    in_deps, levels, (es, es_min_level)
                )
                print(f" at event idx {lines[idx][0]}:")
                for elem in elems_involved:
                    print(f"  elem {elem} is at level {levels[elem]}")
                # TODO(jaeheon) very long leases may not have lifecycle events
                # within history window but still be in the final lease set.
                print(f"  leases: {leases_per_line[idx]}")

    print("... done")

    return 0


if __name__ == "__main__":
    sys.exit(main())
