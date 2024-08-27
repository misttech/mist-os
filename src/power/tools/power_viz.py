# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import itertools
import json
import os
import re
import subprocess
import sys
import zipfile
from collections import deque
from graphlib import TopologicalSorter
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
        if isinstance(curr, dict) and moniker in curr.get("moniker"):
            return curr
        if isinstance(curr, str):
            continue
        for child in curr:
            queue.append(child)
    print(f"No inspect found for moniker {moniker}", file=sys.stderr)
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
    parser.add_argument(
        "--natural",
        action="store_true",
        default=False,
        help="Plots events against a timeline with steady advancement.",
    )
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
    inspect_topology = graph["topology"]
    inspect_events = graph["events"]

    # cache human-readable names
    name_of: dict[str, str] = {}
    for elem_hash in inspect_topology:
        name_of[elem_hash] = inspect_topology[elem_hash]["meta"]["name"]

    # reasonably nice ordering of elements for visual track placement in JS
    sort_gadget: TopologicalSorter[str] = TopologicalSorter()
    for elem_hash in inspect_topology:
        out_edges = inspect_topology[elem_hash]["relationships"]
        for out_elem in out_edges:
            sort_gadget.add(out_elem, elem_hash)
    pb_elem_order = [name_of[h] for h in sort_gadget.static_order()]

    # dict of out edges, to read in JS
    edge_set: dict[str, dict[str, dict[str, str]]] = {}
    for from_vtx in inspect_topology:
        to_vertices = inspect_topology[from_vtx]["relationships"]
        for to_vtx in to_vertices:
            edge_set[name_of[from_vtx]] = {}
        for to_vtx in to_vertices:
            level_map = to_vertices[to_vtx]["meta"]
            edge_set[name_of[from_vtx]][name_of[to_vtx]] = level_map

    inspect_events_keys_sorted = sorted(inspect_events)
    inspect_events_start_y_idx = int(inspect_events_keys_sorted[0])
    inspect_events_end_idx = int(inspect_events_keys_sorted[-1])

    # events: list of event dicts, to read in JS
    events = []
    for inspect_n in sorted(inspect_events):  # list of keys in order
        # each dict:  ordinal, when, what, spec, whom, long (if applicable), level (if applicable)
        curr = inspect_events[inspect_n]
        when = curr["@time"]
        if curr["event"] == "update_key" and curr["key"] == "required_level":
            # level demand
            event = {}
            event["seq"] = inspect_n
            event["when"] = f"{when}"
            event["what"] = "required_level"
            event["spec"] = f'{curr["update"]}'
            event["whom"] = f'{name_of[curr["vertex_id"]]}'
            events.append(event)
        elif curr["event"] == "update_key" and curr["key"] == "current_level":
            # level comply
            event = {}
            event["seq"] = inspect_n
            event["when"] = f"{when}"
            event["what"] = "current_level"
            event["spec"] = f'{curr["update"]}'
            event["whom"] = f'{name_of[curr["vertex_id"]]}'
            # reverse search for matching event
            for r in range(
                int(inspect_n) - 1, inspect_events_start_y_idx - 1, -1
            ):
                prev = inspect_events[str(r)]
                if (
                    prev.get("key") == "required_level"
                    and prev["vertex_id"] == curr["vertex_id"]
                ):
                    dur = (curr["@time"] - prev["@time"]) / 1000  # microseconds
                    event["long"] = f"{dur}"
                    break
            events.append(event)
        elif curr["event"] == "update_key" and curr["key"].startswith(
            "lease_status_"
        ):
            if curr["update"] == "Pending":
                # lease lifecycle
                event = {}
                event["seq"] = inspect_n
                event["when"] = f"{when}"
                event["what"] = "lease_status"
                event["spec"] = "Pend"
                event["whom"] = f'{name_of[curr["vertex_id"]]}'
                # reverse search for matching event
                for r in range(
                    int(inspect_n) - 1, inspect_events_start_y_idx - 1, -1
                ):
                    prev = inspect_events[str(r)]
                    if prev.get("key") == curr["key"]:
                        dur = (
                            curr["@time"] - prev["@time"]
                        ) / 1000  # microseconds
                        event["long"] = f"{dur}"
                if level_re_match := re.search(r"@level_(\d+)", curr["key"]):
                    event["level"] = level_re_match.group()
                events.append(event)
            elif curr["update"] == "Satisfied":
                # lease lifecycle
                event = {}
                event["seq"] = inspect_n
                event["when"] = f"{when}"
                event["what"] = "lease_status"
                event["spec"] = "Satf"
                event["whom"] = f'{name_of[curr["vertex_id"]]}'
                # reverse search for matching event
                for r in range(
                    int(inspect_n) - 1, inspect_events_start_y_idx - 1, -1
                ):
                    prev = inspect_events[str(r)]
                    if prev.get("key") == curr["key"]:
                        dur = (
                            curr["@time"] - prev["@time"]
                        ) / 1000  # microseconds
                        event["long"] = f"{dur}"
                events.append(event)
        elif curr["event"] == "update_key" and curr["key"].startswith("lease_"):
            # lease lifecycle, start
            event = {}
            event["seq"] = inspect_n
            event["when"] = f"{when}"
            event["what"] = "lease"
            event["spec"] = "New"
            event["whom"] = f'{name_of[curr["vertex_id"]]}'
            if level_re_match := re.search(r"\d+", curr["update"]):
                event["level"] = level_re_match.group()
            events.append(event)
        elif curr["event"] == "drop_key" and curr["key"].startswith("lease_"):
            # lease lifecycle, end
            event = {}
            event["seq"] = inspect_n
            event["when"] = f"{when}"
            event["what"] = "lease"
            event["spec"] = "Drop"
            event["whom"] = f'{name_of[curr["vertex_id"]]}'
            events.append(event)
        elif curr["event"] == "add_vertex":
            continue  # topology change, TBD
        elif curr["event"] == "add_edge":
            continue  # topology change, TBD
        elif curr["event"] == "update_key" and curr["key"].isnumeric():
            continue  # topology change, TBD

    # splice in Starnix, SAG, and FSH suspend/resume events

    # extract Starnix's suspend/resume events
    starnix = find_dict(
        moniker="core/starnix_runner/kernels:", json=json_contents
    )
    starnix_events = starnix["payload"]["root"]["suspend_events"]

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

    # add sag and fsh events into 'events' and re-sort. exclude if outside PB's event range.
    history_start = inspect_events[str(inspect_events_start_y_idx)]["@time"]
    history_end = inspect_events[str(inspect_events_end_idx)]["@time"]
    for e in starnix_events.values():
        if key_within(e, "attempted_at_ns", history_start, history_end):
            event = {}
            event["seq"] = "_"
            event["when"] = f'{e["attempted_at_ns"]}'
            event["what"] = "suspend_into_sag"
            event["spec"] = "_"
            event["whom"] = "starnix"
            event["level"] = "0"
            events.append(event)
        elif key_within(e, "resumed_at_ns", history_start, history_end):
            event = {}
            event["seq"] = "_"
            event["when"] = f'{e["resumed_at_ns"]}'
            event["what"] = "resume_from_sag"
            event["spec"] = "_"
            event["whom"] = "starnix"
            event["level"] = "1"
            events.append(event)
    for e in sag_events.values():
        if key_within(e, "suspended", history_start, history_end):
            event = {}
            event["seq"] = "_"
            event["when"] = f'{e["suspended"]}'
            event["what"] = "suspend_into_fsh"
            event["spec"] = "_"
            event["whom"] = "sag"
            event["level"] = "0"
            events.append(event)
        elif key_within(e, "resumed", history_start, history_end):
            event = {}
            event["seq"] = "_"
            event["when"] = f'{e["resumed"]}'
            event["what"] = "resume_from_fsh"
            event["spec"] = "_"
            event["whom"] = "sag"
            event["level"] = "1"
            events.append(event)
    for e in fsh_events.values():
        if key_within(e, "suspended", history_start, history_end):
            event = {}
            event["seq"] = "_"
            event["when"] = f'{e["suspended"]}'
            event["what"] = "suspend_into_zx"
            event["spec"] = "_"
            event["whom"] = "fsh"
            event["level"] = "0"
            events.append(event)
        elif key_within(e, "resumed", history_start, history_end):
            event = {}
            event["seq"] = "_"
            event["when"] = f'{e["resumed"]}'
            event["what"] = "resume_from_zx"
            event["spec"] = "_"
            event["whom"] = "fsh"
            event["level"] = "1"
            events.append(event)
    events = sorted(events, key=lambda event: int(event["when"]))

    full_elem_order = ["starnix"] + pb_elem_order + ["sag", "fsh"]
    numbered_elem_order = dict(
        (h, idx) for idx, h in enumerate(full_elem_order)
    )

    edge_set["starnix-power-mode"]["application_activity"]["1"] = "0"

    history_duration = (
        int(events[-1]["when"]) - int(events[0]["when"])
    ) / 1_000_000_000
    args.output.write(
        HTML_TEMPLATE.replace("<<HISTORY_DURATION>>", f"{history_duration:.0f}")
        .replace("<<EVENTS>>", json.dumps(events))
        .replace("<<ELEM_LIST>>", json.dumps(numbered_elem_order))
        .replace("<<EDGE_SET>>", json.dumps(edge_set))
        .replace("<<NATURAL_TIMELINE_STYLE>>", str(args.natural).lower())
    )

    return 0


HTML_TEMPLATE = """
<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Power Event Flow</title>
    <style type="text/css">
      body {
        font: normal 12px Tahoma
      }
    </style>
</head>
<body>
  <h2>power event visualization</h2>
  <h3>Limitations: only required/current level events that match are displayed.</h3>
  <h3>Event history, observation period <<HISTORY_DURATION>> seconds</h3>
  <div id="diagram" style="margin-top:20px"></div>
  <script type="text/javascript">
    // Visual track order: every element that does something.
    const g_elems = <<ELEM_LIST>>;
    // Topology lookup: set[from_name][to_name] gives a map of power levels.
    const g_edge_set = <<EDGE_SET>>;
    // Events array, already sorted by time.
    const g_data = <<EVENTS>>;

    const g_margin_top = 50;  // fudge px down, from svg top boundary
    const g_margin_left = 350;  // fudge px right, from svg left boundary
    const g_canvas_width = 100 * Object.keys(g_elems).length;
    const g_max_y_px = 20000;

    const g_diagram = document.getElementById("diagram");
    const g_svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    // Ensure svg canvas is large enough.
    g_svg.setAttribute("width", g_canvas_width);
    g_svg.setAttribute("height", g_max_y_px + g_margin_top);
    g_diagram.appendChild(g_svg);

    function px(idx) {
      const g_time_base = parseInt(g_data[0].when);
      const g_time_scale = parseInt(g_data[g_data.length - 1].when) - g_time_base;

      if (<<NATURAL_TIMELINE_STYLE>>) {
        // Events drawn in order and against realtime scale. Conveys sense of scale, harder to read.
        return (parseInt(g_data[idx].when) - g_time_base) / g_time_scale * g_max_y_px;
      } else {
        // Events drawn in order but without time scale. Easy to read, lose sense of duration.
        // This timeline ("logical") is the default.
        return idx / g_data.length * g_max_y_px;
      }
    }

    // SVG machinery for drawing segments from the 'g_data' array.
    // We imagine a Cartesian grid of x and y axes.
    // Orientation: x indices grow to the right. y indices grow downward.
    // - start_x_idx:    x index into imaginary grid
    // - start_x_offset: x fudge factor
    // - start_y_idx:    y index into imaginary grid, start of segment
    // - end_y_idx:      y index into imaginary grid, end of segment
    function draw_segment(start_x_idx, start_x_offset, start_y_idx, end_y_idx, line_style,
                          event_category) {
      const start_y = g_margin_top + px(start_y_idx);
      const end_y = g_margin_top + px(end_y_idx);
      console.log(`y px: ${start_y} - ${end_y}`);
      const track_x = g_margin_left + 50 * start_x_idx + 15 * start_x_offset;

      let color = "black";
      if (event_category === "lease") {
        color = "blue";
      }

      const start_text = document.createElementNS("http://www.w3.org/2000/svg", "text");
      start_text.setAttribute("x", 0);
      start_text.setAttribute("y", start_y);
      const s_evt = g_data[start_y_idx];
      const start_t = parseInt(s_evt.when) / 1e9;
      const start_str = `# ${s_evt.seq}, ${start_t}, ${s_evt.whom}, ${s_evt.what}: ${s_evt.spec}`;
      start_text.textContent = start_str;
      g_svg.appendChild(start_text);

      const end_text = document.createElementNS("http://www.w3.org/2000/svg", "text");
      end_text.setAttribute("x", 0);
      end_text.setAttribute("y", end_y);
      const e_evt = g_data[end_y_idx];
      const end_time = parseInt(e_evt.when) / 1e9;
      const end_str = `# ${e_evt.seq}, ${end_time}, ${e_evt.whom}, ${e_evt.what}: ${e_evt.spec}`;
      end_text.textContent = end_str;
      g_svg.appendChild(end_text);

      const line = document.createElementNS("http://www.w3.org/2000/svg", "line");
      line.setAttribute("x1", track_x);
      line.setAttribute("y1", start_y);
      line.setAttribute("x2", track_x);
      line.setAttribute("y2", end_y);
      line.setAttribute("stroke", color);
      line.setAttribute("stroke-width", "4");
      if (line_style === "dotted") {
        line.setAttribute("stroke-dasharray", "2 4");
        line.setAttribute("stroke", "gray");
      }
      g_svg.appendChild(line);

      const start = document.createElementNS("http://www.w3.org/2000/svg", "circle");
      start.setAttribute("cx", track_x);
      start.setAttribute("cy", start_y);
      start.setAttribute("r", 8);
      start.setAttribute("fill", color);
      if (line_style === "dotted") {
        start.setAttribute("fill", "lightgray");
      }
      g_svg.appendChild(start);

      const end = document.createElementNS("http://www.w3.org/2000/svg", "circle");
      end.setAttribute("cx", track_x);
      end.setAttribute("cy", end_y);
      end.setAttribute("r", 8);
      end.setAttribute("fill", color);
      if (line_style === "dotted") {
        end.setAttribute("fill", "lightgray");
      }
      g_svg.appendChild(end);
    }

    function draw_dep(start_x_idx, start_x_offset, start_y_idx, end_x_idx, end_x_offset,
                      end_y_idx) {
      const start_x = g_margin_left + 50 * start_x_idx + 15 * start_x_offset;
      const start_y = g_margin_top + px(start_y_idx);
      const end_x = g_margin_left + 50 * end_x_idx + 15 * end_x_offset;
      const end_y = g_margin_top + px(end_y_idx);

      const line = document.createElementNS("http://www.w3.org/2000/svg", "line");
      line.setAttribute("x1", start_x);
      line.setAttribute("y1", start_y);
      line.setAttribute("x2", end_x);
      line.setAttribute("y2", end_y);
      line.setAttribute("stroke", "black");
      line.setAttribute("stroke-width", "2");
      g_svg.appendChild(line);
    }

    function draw_region(start_y_idx, end_y_idx) {
      const start_y_px = px(start_y_idx);
      const end_y_px = px(end_y_idx);
      const y = g_margin_top + start_y_px;
      const height = end_y_px - start_y_px;
      console.log(`>>>> y px: ${start_y_px} - ${end_y_px}`);

      const rect = document.createElementNS("http://www.w3.org/2000/svg", "rect");
      rect.setAttribute("x", 0);
      rect.setAttribute("y", y);
      rect.setAttribute("width", g_canvas_width);
      rect.setAttribute("height", height);
      rect.setAttribute("fill", "mistyrose");
      g_svg.appendChild(rect);
    }

    let segments = [], sx = 0;
    for (let i = 0; i < g_data.length; ++i) {
      // Ensure this is a "start" event.
      if (g_data[i].what === "current_level"
          || (g_data[i].what === "lease_status" && g_data[i].spec === "Satf")) {
        continue;  // These are always "tail" events.
      }
      // Find matching "end" event's index as 'j'.
      let j = i + 1;
      let line_style = "";
      let event_category = "";
      if (g_data[i].what === "required_level") {
        for (; j < g_data.length; ++j) {
          if (g_data[j].whom === g_data[i].whom
              && g_data[j].what ===  "current_level"
              && g_data[j].spec === g_data[i].spec) {
            event_category = "level";
            break;  // "proper" match found
          } else if (g_data[j].whom === g_data[i].whom
              && g_data[j].what ===  "required_level"
              && g_data[j].spec !== g_data[i].spec) {
            event_category = "level";
            line_style = "dotted";  // request superseded by another required_level
            break;
          }
        }
      } else if (g_data[i].what === "lease" && g_data[i].spec === "New") {
        // New -> Drop
        // New -> Satf (but then elide Pend -> Satf)
        for (; j < g_data.length; ++j) {
          if (g_data[j].whom === g_data[i].whom
              && g_data[j].what ===  "lease_status"
              && g_data[j].spec === "Pend") {
            g_data[j].used = true;  // we can skip this one, reduce visual clutter
            continue;
          } else if (g_data[j].whom === g_data[i].whom
              && g_data[j].what ===  "lease_status"
              && g_data[j].spec === "Satf") {
            g_data[j].used = true;
            event_category = "lease";
            break;
          } else if (g_data[j].whom === g_data[i].whom
              && g_data[j].what ===  "lease"
              && g_data[j].spec === "Drop") {
            g_data[j].used = true;
            event_category = "lease";
            break;
          }
        }
      } else if (g_data[i].what === "lease_status" && g_data[i].spec === "Pend") {
        if (g_data[i].used) {
          continue;  // skip to next event
        }
        // Pend -> Satf
        // Pend -> Drop
        for (; j < g_data.length; ++j) {
          if (g_data[j].whom === g_data[i].whom
              && g_data[j].what ===  "lease_status"
              && g_data[j].spec === "Satf"
              && !g_data[j].used) {
            event_category = "lease";
            break;
          } else if (g_data[j].whom === g_data[i].whom
              && g_data[j].what ===  "lease"
              && g_data[j].spec === "Drop"
              && !g_data[j].used) {
            g_data[j].used = true;
            event_category = "lease";
            line_style = "dotted";  // lease was never satisfied
            break;
          }
        }
      } else if (g_data[i].what === "lease" && g_data[i].spec === "Drop") {
        if (g_data[i].used) {
          continue;  // skip to next event
        }
        j = i;  // Draw a dot, not a line
        event_category = "lease";
      } else if (g_data[i].what.includes("suspend_into") ||
                 g_data[i].what.includes("resume_from")) {
        j = i;  // Draw a dot, not a line
        event_category = "level";
      }

      if (j == g_data.length) {
        continue;  // No end-match found, continue to next event.
      }

      // Find and sort all conflicting previous segments due to chronological overlap.
      let conflict_set = [];
      for (let tx = 0; tx < sx; ++tx) {
        if (g_data[segments[tx].start].whom === g_data[i].whom
            && segments[tx].start <= i
            && i <= segments[tx].end) {
          conflict_set.push(segments[tx].offset);
        }
      }
      conflict_set.sort();
      // Find lowest non-conflicting track number as offset factor 'k'.
      let k = 0;
      if ((conflict_set.length - 1) == conflict_set[conflict_set.length - 1]) {
        k = conflict_set.length;  // Conflict set is packed, use fresh track.
      } else {
        for (; k < conflict_set[conflict_set.length - 1]; ++k) {
          if (k != conflict_set[k]) {
            break;  // Found the first "hole" in the sorted conflict set.
          }
        }
      }

      // Store this segment.
      segments[sx++] = {
        "start": i,
        "end": j,
        "track": g_elems[g_data[i].whom],
        "offset": k,
        "event_category": event_category,
        "line_style": line_style,
      };
    }

    // Draw background color for "suspend -> region" regions.
    for (let sx = 0; sx < segments.length; ++sx) {
      const s = segments[sx];
      const sd = g_data[s.start];
      if (sd.whom === "starnix" && sd.what === "suspend_into_sag") {
        for (let tx = sx + 1; tx < segments.length; ++tx) {
          const t = segments[tx];
          const td = g_data[t.start];
          if (td.whom === "starnix" && td.what === "resume_from_sag") {
            draw_region(s.start, t.end);
            break;
          }
        }
      }
    }
    // Draw the segments on the tracks.
    for (s of segments) {
      draw_segment(s.track, s.offset, s.start, s.end, s.line_style, s.event_category);
    }
    // Draw the dependencies between segments.
    for (let sx = 0; sx < segments.length; ++sx) {
      const r = segments[sx];
      const rd = g_data[r.start];
      const re = g_data[r.end];
      if ((rd.what === "lease" && rd.spec === "New")
          || (rd.what === "lease_status" && rd.spec === "Pend")) {
        // lease.new -> required_level, lease.pending -> required_level
        for (let tx = sx + 1; tx < segments.length; ++tx) {
          const t = segments[tx];
          const td = g_data[t.start];
          if (r.start <= t.start && t.start <= r.end
              && td.what === "required_level" && td.spec === rd.level
              && rd.whom in g_edge_set
              && td.whom in g_edge_set[rd.whom]
              && rd.level in g_edge_set[rd.whom][td.whom]
              && td.spec === g_edge_set[rd.whom][td.whom][rd.level]) {
            draw_dep(r.track, r.offset, r.start, t.track, t.offset, t.start);
          }
        }
      } else if (rd.whom === "starnix" && rd.what === "suspend_into_sag") {
        // starnix.suspend -> starnix-power-mode.drop
        for (let tx = sx + 1; tx < segments.length; ++tx) {
          // find starnix-power-mode Drop
          const t = segments[tx];
          const td = g_data[t.start];
          if (td.whom === "starnix-power-mode" && td.what === "lease" && td.spec === "Drop") {
            draw_dep(r.track, r.offset, r.start, t.track, t.offset, t.start);
            break;
          }
        }
      } else if (re.whom === "starnix-power-mode" && re.what === "current_level" &&
                 re.spec === "1") {
        // starnix-power-mode.current_level.1 -> application_activity.required_level.0
        for (let tx = sx + 1; tx < segments.length; ++tx) {
          const t = segments[tx];
          const td = g_data[t.start];
          if (td.whom === "application_activity" && td.spec === "0") {
            draw_dep(r.track, r.offset, r.end, t.track, t.offset, t.start);
            break;
          }
        }
      } else if (re.whom === "execution_state" && re.what === "current_level" &&
                 re.spec === "0") {
        // execution_state.current_level.0 -> sag.suspend
        for (let tx = sx + 1; tx < segments.length; ++tx) {
          const t = segments[tx];
          const td = g_data[t.start];
          if (td.whom === "sag" && td.what === "suspend_into_fsh") {
            draw_dep(r.track, r.offset, r.end, t.track, t.offset, t.start);
            break;
          }
        }
      } else if (re.whom === "sag" && re.what === "suspend_into_fsh") {
        // sag.suspend -> fsh.suspend
        for (let tx = sx + 1; tx < segments.length; ++tx) {
          const t = segments[tx];
          const td = g_data[t.start];
          if (td.whom === "fsh" && td.what === "suspend_into_zx") {
            draw_dep(r.track, r.offset, r.end, t.track, t.offset, t.start);
            break;
          }
        }
      } else if (rd.whom === "fsh" && rd.what === "resume_from_zx") {
        // fsh.resume -> sag.resume
        for (let tx = sx + 1; tx < segments.length; ++tx) {
          const t = segments[tx];
          const td = g_data[t.start];
          if (td.whom === "sag" && td.what === "resume_from_fsh") {
            draw_dep(r.track, r.offset, r.end, t.track, t.offset, t.start);
            break;
          }
        }
      } else if (re.whom === "starnix-power-mode" && re.what === "current_level" &&
                 re.spec === "4") {
        // starnix-power-mode.current_level.4 -> starnix.resume
        for (let tx = sx + 1; tx < segments.length; ++tx) {
          const t = segments[tx];
          const td = g_data[t.start];
          if (td.whom === "starnix" && td.what === "resume_from_sag") {
            draw_dep(r.track, r.offset, r.end, t.track, t.offset, t.start);
            break;
          }
        }
      }
    }
  </script>
</body>
</html>
"""


if __name__ == "__main__":
    sys.exit(main())
