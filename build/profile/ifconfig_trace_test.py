#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import datetime
import unittest
from pathlib import Path
from unittest import mock

import ifconfig_trace
import trace_tools

_SAMPLE_IFCONFIG_TIMES = [
    datetime.datetime(
        year=2024,
        month=10,
        day=17,
        hour=21,
        minute=9,
        second=27,
    ),
    datetime.datetime(
        year=2024,
        month=10,
        day=17,
        hour=21,
        minute=9,
        second=32,  # delta: 5s
    ),
]

_SAMPLE_IFCONFIG_DATA = """TIME: 2024-10-17 21:09:27
docker0: flags=4099<UP,BROADCAST,MULTICAST>  mtu 1500
        inet 172.17.0.1  netmask 255.255.0.0  broadcast 172.17.255.255
        ether 02:42:73:68:8f:48  txqueuelen 0  (Ethernet)
        RX packets 0  bytes 0 (0.0 B)
        RX errors 0  dropped 0  overruns 0  frame 0
        TX packets 0  bytes 0 (0.0 B)
        TX errors 0  dropped 0 overruns 0  carrier 0  collisions 0

ens4: flags=4163<UP,BROADCAST,RUNNING,MULTICAST>  mtu 1460
        inet 192.168.0.43  netmask 255.255.255.255  broadcast 0.0.0.0
        inet6 fe80::8069:a383:84ff:3e2a  prefixlen 64  scopeid 0x20<link>
        ether 42:01:c0:a8:00:2b  txqueuelen 1000  (Ethernet)
        RX packets 304807635  bytes 857399212957 (798.5 GiB)
        RX errors 0  dropped 0  overruns 0  frame 0
        TX packets 226373105  bytes 189976016591 (176.9 GiB)
        TX errors 0  dropped 0 overruns 0  carrier 0  collisions 0

TIME: 2024-10-17 21:09:32
docker0: flags=4099<UP,BROADCAST,MULTICAST>  mtu 1500
        inet 172.17.0.1  netmask 255.255.0.0  broadcast 172.17.255.255
        ether 02:42:73:68:8f:48  txqueuelen 0  (Ethernet)
        RX packets 0  bytes 0 (0.0 B)
        RX errors 0  dropped 0  overruns 0  frame 0
        TX packets 0  bytes 0 (0.0 B)
        TX errors 0  dropped 0 overruns 0  carrier 0  collisions 0

ens4: flags=4163<UP,BROADCAST,RUNNING,MULTICAST>  mtu 1460
        inet 192.168.0.43  netmask 255.255.255.255  broadcast 0.0.0.0
        inet6 fe80::8069:a383:84ff:3e2a  prefixlen 64  scopeid 0x20<link>
        ether 42:01:c0:a8:00:2b  txqueuelen 1000  (Ethernet)
        RX packets 304809235  bytes 857399495522 (798.5 GiB)
        RX errors 0  dropped 0  overruns 0  frame 0
        TX packets 226374234  bytes 189976297294 (176.9 GiB)
        TX errors 0  dropped 0 overruns 0  carrier 0  collisions 0

"""

_EXPECTED_IFCONFIG_DATA_STREAM = [
    ifconfig_trace.TransmissionData(
        interface_name="docker0",
        rx_packets=0,
        rx_bytes=0,
        tx_packets=0,
        tx_bytes=0,
        timestamp=_SAMPLE_IFCONFIG_TIMES[0],
    ),
    ifconfig_trace.TransmissionData(
        interface_name="ens4",
        rx_packets=304807635,
        rx_bytes=857399212957,
        tx_packets=226373105,
        tx_bytes=189976016591,
        timestamp=_SAMPLE_IFCONFIG_TIMES[0],
    ),
    ifconfig_trace.TransmissionData(
        interface_name="docker0",
        rx_packets=0,
        rx_bytes=0,
        tx_packets=0,
        tx_bytes=0,
        timestamp=_SAMPLE_IFCONFIG_TIMES[1],
    ),
    ifconfig_trace.TransmissionData(
        interface_name="ens4",
        rx_packets=304809235,
        rx_bytes=857399495522,
        tx_packets=226374234,
        tx_bytes=189976297294,
        timestamp=_SAMPLE_IFCONFIG_TIMES[1],
    ),
]


class IfConfigEntryTests(unittest.TestCase):
    def test_chrome_trace_events_json(self) -> None:
        ifconfig_entry = _EXPECTED_IFCONFIG_DATA_STREAM[0]
        events = list(
            ifconfig_entry.chrome_trace_events_json(
                ifconfig_entry.timestamp, ifconfig_entry
            )
        )
        self.assertEqual(len(events), 4)  # {tx,rx}_{packets,bytes}

    def test_parse_ifconfig_loop_output(self) -> None:
        lines = _SAMPLE_IFCONFIG_DATA.splitlines()
        entries = list(ifconfig_trace._parse_ifconfig_loop_output(iter(lines)))
        self.assertEqual(entries, _EXPECTED_IFCONFIG_DATA_STREAM)

    def test_print_chrome_trace_json_empty(self) -> None:
        fmt = trace_tools.Formatter()
        lines = list(ifconfig_trace.print_chrome_trace_json(fmt, iter([])))
        self.assertEqual(lines, [])

    def test_print_chrome_trace_json_nonempty(self) -> None:
        fmt = trace_tools.Formatter()
        lines = list(
            ifconfig_trace.print_chrome_trace_json(
                fmt, iter(_EXPECTED_IFCONFIG_DATA_STREAM)
            )
        )

        def expected_event(name: str, time: int, value: int) -> str:
            return f"""{{"name": "{name}", "cat": "network", "ph": "C", "pid": 1, "tid": 1, "ts": {time}, "args": {{"count": "{value}"}} }},"""

        self.assertEqual(
            lines,
            [
                expected_event("docker0.rx.packets", 0, 0),
                expected_event("docker0.rx.bytes", 0, 0),
                expected_event("docker0.tx.packets", 0, 0),
                expected_event("docker0.tx.bytes", 0, 0),
                expected_event("ens4.rx.packets", 0, 0),
                expected_event("ens4.rx.bytes", 0, 0),
                expected_event("ens4.tx.packets", 0, 0),
                expected_event("ens4.tx.bytes", 0, 0),
                expected_event("docker0.rx.packets", 5000000, 0),
                expected_event("docker0.rx.bytes", 5000000, 0),
                expected_event("docker0.tx.packets", 5000000, 0),
                expected_event("docker0.tx.bytes", 5000000, 0),
                expected_event("ens4.rx.packets", 5000000, 1600),
                expected_event("ens4.rx.bytes", 5000000, 282565),
                expected_event("ens4.tx.packets", 5000000, 1129),
                expected_event("ens4.tx.bytes", 5000000, 280703),
            ],
        )


class MainTests(unittest.TestCase):
    def test_parse_and_print(self) -> None:
        argv = ["ifconfig_loop.log"]
        with mock.patch.object(
            Path, "read_text", return_value=_SAMPLE_IFCONFIG_DATA
        ) as mock_read:
            returncode = ifconfig_trace.main(argv)
        self.assertEqual(returncode, 0)
        mock_read.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
