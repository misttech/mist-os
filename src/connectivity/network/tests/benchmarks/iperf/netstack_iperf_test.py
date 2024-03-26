# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import concurrent.futures
import ipaddress
import logging
import json
import os
import statistics
import time
from enum import Enum
from typing import Any, Self

import honeydew
from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from mobly import asserts, test_runner
from perf_publish import publish
from perf_test_utils import utils
from trace_processing import trace_importing
from trace_processing import trace_metrics
from trace_processing import trace_model
from trace_processing import trace_utils
from trace_processing.metrics import cpu

# The first TCP/UDP port number that the Fuchsia side will listen on.
FIRST_LISTEN_PORT: int = 9001

_LOGGER: logging.Logger = logging.getLogger(__name__)


class Protocol(Enum):
    TCP = 1
    UDP = 2

    @classmethod
    def from_str(cls, value: str) -> "Protocol":
        value = value.lower()
        if value == "tcp":
            return Protocol.TCP
        if value == "udp":
            return Protocol.UDP
        raise ValueError(f"Invalid Protocol variant string: {value}")

    def __str__(self) -> str:
        if self == Protocol.TCP:
            return "TCP"
        if self == Protocol.UDP:
            return "UDP"
        raise ValueError("Unknown Protocol variant")


class Direction(Enum):
    DEVICE_TO_HOST = 1
    HOST_TO_DEVICE = 2
    LOOPBACK = 3

    @classmethod
    def from_str(cls, value: str) -> "Direction":
        value = value.lower()
        if value == "send":
            return Direction.DEVICE_TO_HOST
        if value == "recv":
            return Direction.HOST_TO_DEVICE
        if value == "loopback":
            return Direction.LOOPBACK
        raise ValueError(f"Invalid Direction variant string: {value}")

    def __str__(self) -> str:
        if self == Direction.DEVICE_TO_HOST:
            return "send"
        if self == Direction.HOST_TO_DEVICE:
            return "recv"
        if self == Direction.LOOPBACK:
            return "loopback"
        raise ValueError("Unknown Direction variant")


class Stats:
    def __init__(
        self,
        protocol: Protocol,
        direction: Direction,
        message_size: int,
        flows: int,
    ) -> None:
        self._protocol = protocol
        self._direction = direction
        self._message_size = message_size
        self._flows = flows
        self._throughputs: list[float] = []
        self._packets: int = 0
        self._lost_packets: int = 0
        self._jitter_weighted: float = 0.0

    def add(self, iperf_results: dict[str, Any]) -> None:
        setup = iperf_results["start"]["test_start"]
        # Verify iperf parameters are as we'd expect.
        asserts.assert_equal(setup["protocol"], str(self._protocol))
        asserts.assert_equal(setup["blksize"], self._message_size)

        end = iperf_results["end"]
        if self._protocol == Protocol.TCP:
            if self._direction == Direction.DEVICE_TO_HOST:
                self._throughputs.append(end["sum_sent"]["bits_per_second"])
            else:
                self._throughputs.append(end["sum_received"]["bits_per_second"])
            return
        if self._direction == Direction.DEVICE_TO_HOST:
            self._throughputs.append(end["sum"]["bits_per_second"])
            return
        # For UDP, there is no sum_received record, but we gather the
        # receiver information from server-output.
        receiver = iperf_results["server_output_json"]["end"]["sum"]
        # TODO(https://github.com/esnet/iperf/issues/754): Remove the following
        # once iperf calculates throughput correctly when the server is the
        # receiver. In the meantime, derive the value from the other stats.
        self._throughputs.append(
            (self._message_size * 8) * receiver["packets"] / receiver["seconds"]
        )
        self._packets += receiver["packets"]
        self._lost_packets += receiver["lost_packets"]
        # Note that in order to compute the average jitter of packets
        # across all flows, the jitter for each flow must be weighted by
        # the packet count of said flow to produce the total jitter across
        # all packets, which when divided by the number of packets yields
        # the correct statistic.
        self._jitter_weighted += receiver["packets"] * receiver["jitter_ms"]

    def results(self, cpu_percentages: list[float]) -> list[dict[str, Any]]:
        asserts.assert_equal(self._flows, len(self._throughputs))
        label: str = (
            f"{self._protocol}/{self._direction}/{self._message_size}bytes"
        )
        if self._flows > 1:
            label += f"/{self._flows}flows"
        results: list[dict[str, Any]] = []
        throughput: float = sum(self._throughputs)
        results.append(generate_result(label, "bits_per_second", [throughput]))
        results.append(generate_result(label, "CPU", cpu_percentages))
        if (
            self._protocol != Protocol.TCP
            and self._direction != Direction.DEVICE_TO_HOST
        ):
            results.append(
                generate_result(label, "lost_packets", [self._lost_packets])
            )
            results.append(
                generate_result(
                    label,
                    "lost_percent",
                    [
                        self._lost_packets
                        / (self._lost_packets + self._packets)
                        * 100
                    ],
                )
            )
            results.append(
                generate_result(
                    label, "jitter_ms", [self._jitter_weighted / self._packets]
                )
            )
        if self._flows > 1:
            std_dev = statistics.stdev(self._throughputs)
            mean = statistics.mean(self._throughputs)
            results.append(
                generate_result(
                    label,
                    "bits_per_second_coefficient_of_variation",
                    [std_dev / mean * 100],
                )
            )
        return results


UNIT_MAP = {
    "bits_per_second": "bits/second",
    "bits_per_second_coefficient_of_variation": "percent",
    "lost_packets": "count_smallerIsBetter",
    "lost_percent": "percent",
    "jitter_ms": "milliseconds",
    "CPU": "percent",
}


def generate_result(label: str, key: str, values: list[Any]) -> dict[str, Any]:
    unit: str = UNIT_MAP.get(key, "unknown")
    return {
        "label": f"{label}/{key}",
        "test_suite": "fuchsia.netstack.iperf_benchmarks",
        "unit": unit,
        "values": values,
    }


class IperfServer:
    def __init__(self, port: int, ssh: honeydew.transports.ssh.SSH) -> None:
        self._process = ssh.popen(
            f"iperf3 --server --port {port} --json", get_ssh_addr_timeout=None
        )

    def dump_output_to_file(self, path: str) -> None:
        self._process.kill()
        output, err = self._process.communicate()
        if err:
            _LOGGER.warn(f"Server wrote errors: {err}")
        # NOTE: this file contains a set of JSON objects (not a list of objects, just a bunch of
        # JSON objects). The first one is the one we used to check that the connection had been
        # established. Consider removing that one as it's a test implementation detail.
        with open(path, "wb") as f:
            f.write(output)


class NetstackIperfTest(fuchsia_base_test.FuchsiaBaseTest):  # type: ignore[misc]
    def setup_test(self) -> None:
        super().setup_test()
        self._device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        self._protocol = Protocol.from_str(self.user_params["protocol"])
        self._direction = Direction.from_str(self.user_params["direction"])
        self._label = self.user_params["label"]

    def test_iperf(self) -> None:
        executor = concurrent.futures.ThreadPoolExecutor(max_workers=5)
        self._wait_system_metrics_daemon_start()
        try:
            self._run_iperf_client_tests(executor)
        finally:
            self._cleanup_iperf_tasks()

    def _wait_system_metrics_daemon_start(self) -> None:
        for i in range(10):
            with self._device.tracing.trace_session(
                categories=["system_metrics"],
                download=True,
                directory=self.test_case_path,
                trace_file="trace.fxt",
            ):
                # Do nothing for sometime to let system_metrics to be logged.
                time.sleep(10)
            cpu_results = self._get_cpu_results(
                os.path.join(self.test_case_path, "trace.fxt")
            )
            if len(cpu_results) > 0:
                return
        raise RuntimeError(
            "Failed to retrieve CPU stats from system_metrics daemon"
        )

    def _run_iperf_client_tests(
        self,
        executor: concurrent.futures.ThreadPoolExecutor,
    ) -> None:
        server_ip: ipaddress.IPv4Address | ipaddress.IPv6Address = (
            ipaddress.ip_address("127.0.0.1")
            if self._direction == Direction.LOOPBACK
            # TODO(https://fxbug.dev/42124566): Currently, we are using the link used for ssh to
            # also inject data traffic. This is prone to interference to ssh and to the tests.
            # On NUC7, we can use a separate usb-ethernet interface for the test traffic.
            else self._device.ssh.get_target_address(timeout=None).ip
        )
        results: list[dict[str, Any]] = []
        for message_size in [64, 1024, 1400]:
            for flows in [1, 2, 4]:
                # Disable the ethernet_udp_recv 64-byte multi-flow test cases because (as of
                # writing) they are very flaky, most likely due to the netstack or network driver
                # dropping packets under high load (see https://fxbug.dev/42085351).
                if (
                    self._protocol == Protocol.UDP
                    and self._direction == Direction.HOST_TO_DEVICE
                    and message_size == 64
                    and flows > 1
                ):
                    continue
                servers = asyncio.run(
                    self._start_iperf3_servers(executor, flows)
                )
                try:
                    with self._device.tracing.trace_session(
                        categories=["system_metrics"],
                        download=True,
                        directory=self.test_case_path,
                        trace_file="trace.fxt",
                    ):
                        result_files = asyncio.run(
                            self._execute_iperf3_commands(
                                executor,
                                flows,
                                server_ip,
                                message_size,
                            )
                        )
                    cpu_results = self._get_cpu_results(
                        os.path.join(self.test_case_path, "trace.fxt")
                    )
                    asserts.assert_equal(len(cpu_results), 1)
                    cpu_percentages: list[float] = cpu_results[0].values
                    results += self._iperf_results_to_fuchsiaperf(
                        result_files,
                        cpu_percentages,
                        message_size,
                    )
                finally:
                    self._cleanup_iperf_tasks()
                    for i, server in enumerate(servers):
                        server.dump_output_to_file(
                            os.path.join(
                                self.test_case_path,
                                f"iperf_server_{message_size}_{flows}_{i}.json",
                            )
                        )
        path = os.path.join(
            self.test_case_path, "netstack_iperf_results.fuchsiaperf.json"
        )
        with open(path, "w") as f:
            json.dump(results, f)
        publish.publish_fuchsiaperf(
            [path], f"fuchsia.netstack.iperf_benchmarks.{self._label}.txt"
        )

    def _get_cpu_results(self, path: str) -> list[trace_metrics.TestCaseResult]:
        json_trace_file: str = trace_importing.convert_trace_file_to_json(path)
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            json_trace_file
        )
        return cpu.metrics_processor(model, {})

    async def _start_iperf3_servers(
        self, executor: concurrent.futures.ThreadPoolExecutor, flows: int
    ) -> list[IperfServer]:
        loop = asyncio.get_running_loop()
        client_futures = []
        for i in range(flows):
            client_futures.append(
                loop.run_in_executor(
                    executor,
                    self._start_iperf3_server,
                    i,
                )
            )
        results, pending = await asyncio.wait(
            client_futures, return_when=asyncio.ALL_COMPLETED
        )
        asserts.assert_equal(len(pending), 0)
        asserts.assert_equal(len(results), flows)
        return [result.result() for result in results]

    def _start_iperf3_server(self, index: int) -> IperfServer:
        port: int = FIRST_LISTEN_PORT + index
        server = IperfServer(port, self._device.ssh)
        while True:
            try:
                output = self._device.ssh.run(
                    f"iperf3 -n 1 -c 127.0.0.1 -p {port}",
                    timeout=None,
                    get_ssh_addr_timeout=None,
                )
                asserts.assert_not_in(
                    "iperf3: error - unable to connect to server: Connection refused",
                    output,
                )
                output = output.strip()
                asserts.assert_true(
                    output.startswith(
                        f"Connecting to host 127.0.0.1, port {port}"
                    ),
                    "output has expected beginning",
                )
                asserts.assert_in(f"connected to 127.0.0.1 port {port}", output)
                asserts.assert_true(
                    output.endswith("iperf Done."), "output has expected end"
                )
                return server
            except honeydew.errors.SSHCommandError as err:
                time.sleep(1)

    async def _execute_iperf3_commands(
        self,
        executor: concurrent.futures.ThreadPoolExecutor,
        flows: int,
        server_ip: ipaddress.IPv4Address | ipaddress.IPv6Address,
        message_size: int,
    ) -> list[str]:
        loop = asyncio.get_running_loop()
        bw_value = (
            # For localhost test, use maximum possible bandwidth.
            "0"
            if self._direction == Direction.LOOPBACK
            # TODO(https://fxbug.dev/42124566): Until we define separate link for ssh and data,
            # enforce a < 1Gbps rate on NUC7. After the bug is resolved, this can
            # be changed to '0' which means as much as the system and link can
            # transmit.
            else f"{100 // flows}M"
        )
        protocol_option: str = "--udp" if self._protocol == Protocol.UDP else ""
        dir_option: str = (
            "--reverse" if self._direction == Direction.DEVICE_TO_HOST else ""
        )
        command_args = [
            "--client",
            f"{server_ip}",
            "--length",
            f"{message_size}",
            "--json",
            protocol_option,
            "--bitrate",
            bw_value,
            dir_option,
            "--get-server-output",
        ]
        tasks = []
        for i in range(flows):
            cmd_args = command_args + ["--port", f"{FIRST_LISTEN_PORT + i}"]
            if self._direction == Direction.LOOPBACK:
                tasks.append(
                    loop.run_in_executor(
                        executor,
                        self._run_target_iperf3_command,
                        cmd_args,
                        f"iperf_client_{i}.json",
                    )
                )
            else:
                tasks.append(
                    asyncio.create_task(
                        self._run_host_iperf3_command(
                            cmd_args, f"iperf_client_{i}.json"
                        )
                    )
                )
        (result_files, pending) = await asyncio.wait(
            tasks, return_when=asyncio.ALL_COMPLETED
        )
        asserts.assert_equal(len(pending), 0)
        asserts.assert_equal(len(result_files), flows)
        return [f.result() for f in result_files]

    def _run_target_iperf3_command(
        self, cmd_args: list[str], output_filename: str
    ) -> str:
        args = " ".join(cmd_args)
        output = self._device.ssh.run(
            f"iperf3 {args}",
            timeout=None,
            get_ssh_addr_timeout=None,
        )
        # We're writing to a file so that the output is available for troubleshooting purposes when
        # run in Infra.
        results_path = os.path.join(
            self.test_case_path,
            output_filename,
        )
        with open(results_path, "w") as f:
            f.write(output)
        return results_path

    async def _run_host_iperf3_command(
        self, cmd_args: list[str], output_filename: str
    ) -> str:
        # Run iperf3 client from the host-tools
        host_path = os.path.join(
            utils.get_associated_runtime_deps_dir(__file__), "iperf3"
        )
        process = await asyncio.create_subprocess_exec(
            host_path,
            *cmd_args,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await process.communicate()
        stdout_str = stdout.decode("utf-8")
        stderr_str = stderr.decode("utf-8")

        asserts.assert_equal(
            process.returncode,
            0,
            f"output: {stdout_str} stderr: {stderr_str}",
        )
        results_path = os.path.join(
            self.test_case_path,
            output_filename,
        )
        with open(results_path, "wb") as f:
            f.write(stdout)
        return results_path

    def _iperf_results_to_fuchsiaperf(
        self,
        result_files: list[str],
        cpu_percentages: list[float],
        message_size: int,
    ) -> list[dict[str, Any]]:
        flows: int = len(result_files)
        stats: Stats = Stats(
            self._protocol, self._direction, message_size, flows
        )
        for result_file in result_files:
            with open(result_file, "r") as f:
                iperf_results = json.load(f)
            stats.add(iperf_results)
        return stats.results(cpu_percentages)

    def _cleanup_iperf_tasks(self) -> None:
        try:
            self._device.ssh.run(
                "killall iperf3",
                timeout=None,
                get_ssh_addr_timeout=None,
            )
        except honeydew.errors.SSHCommandError as err:
            # killall returns -1 and prints "no tasks found" in its output
            # when there's no tasks to kill.
            pass


if __name__ == "__main__":
    test_runner.main()
