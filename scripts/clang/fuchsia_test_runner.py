#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
The script for running LLVM Unit Test running for Fuchsia.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
import fcntl
import hashlib
import glob
import json
import logging
import os
import platform
import shutil
import subprocess
import struct
import sys
from pathlib import Path
from typing import ClassVar, List, Optional


def check_call_with_logging(
    args, *, stdout_handler, stderr_handler, check=True, text=True, **kwargs
):
    stdout_handler(f"Subprocess: {args}")
    with subprocess.Popen(
        args,
        text=text,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        **kwargs,
    ) as process:
        with ThreadPoolExecutor(max_workers=2) as executor:

            def exhaust_pipe(handler, pipe):
                for line in pipe:
                    handler(line.rstrip())

            executor_out = executor.submit(
                exhaust_pipe, stdout_handler, process.stdout
            )
            executor_err = executor.submit(
                exhaust_pipe, stderr_handler, process.stderr
            )
            executor_out.result()
            executor_err.result()
    retcode = process.poll()
    if check and retcode:
        subprocess.CalledProcessError(retcode, process.args)
    return subprocess.CompletedProcess(process.args, retcode)


def atomic_link(link: Path, target: Path):
    link_dir = link.parent
    os.makedirs(link_dir, exist_ok=True)
    link_file = link.name
    tmp_file = link_dir.joinpath(link_file + "_tmp")
    os.link(target, tmp_file)
    try:
        os.rename(tmp_file, link)
    except Exception as e:
        raise e
    finally:
        if tmp_file.exists():
            os.remove(tmp_file)


@dataclass
class TestEnvironment:
    build_dir: str
    sdk_dir: str
    target: str
    toolchain_dir: str
    abi_revision: str
    local_pb_path: str
    use_local_pb: bool
    verbose: bool = False

    triple_to_arch_map = {
        "x86_64-fuchsia": "x64",
        "aarch64-fuchsia": "arm64",
        "riscv64-fuchsia": "riscv64",
    }

    env_logger = logging.getLogger("env")
    subprocess_logger = logging.getLogger("env.subprocess")
    __tmp_dir = None

    @staticmethod
    def tmp_dir() -> Path:
        if TestEnvironment.__tmp_dir:
            return TestEnvironment.__tmp_dir
        tmp_dir = os.environ.get("TEST_TOOLCHAIN_TMP_DIR")
        if tmp_dir is not None:
            TestEnvironment.__tmp_dir = Path(tmp_dir).absolute()
        else:
            TestEnvironment.__tmp_dir = Path(__file__).parent.joinpath("tmp~")
        return TestEnvironment.__tmp_dir

    @staticmethod
    def triple_to_arch(triple) -> str:
        elems = triple.split("-")
        if len(elems) < 2:
            raise Exception(f"Unrecognized target triple {triple}")
        triple_s = f"{elems[0]}-{elems[1]}"
        if triple_s not in TestEnvironment.triple_to_arch_map:
            raise Exception(f"Unrecognized target triple {triple}")
        return TestEnvironment.triple_to_arch_map[triple_s]

    @classmethod
    def env_file_path(cls) -> Path:
        return cls.tmp_dir().joinpath("test_env.json")

    @classmethod
    def from_args(cls, args):
        return cls(
            str(Path(args.build_dir).absolute()),
            str(Path(args.sdk).absolute()),
            args.target,
            str(Path(args.toolchain_dir).absolute()),
            args.abi_revision,
            args.local_product_bundle_path,
            args.use_local_product_bundle_if_exists,
            verbose=args.verbose,
        )

    @classmethod
    def read_from_file(cls):
        with open(cls.env_file_path(), encoding="utf-8") as f:
            test_env = json.load(f)
            return cls(
                str(Path(test_env["build_dir"])),
                str(Path(test_env["sdk_dir"])),
                test_env["target"],
                str(Path(test_env["toolchain_dir"])),
                test_env["abi_revision"],
                test_env["local_pb_path"],
                test_env["use_local_pb"],
                verbose=test_env["verbose"],
            )

    def build_id(self, binary):
        llvm_readelf = Path(self.toolchain_dir).joinpath("bin", "llvm-readelf")
        process = subprocess.run(
            args=[
                llvm_readelf,
                "-n",
                "--elf-output-style=JSON",
                binary,
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        if process.returncode:
            self.env_logger.error(
                f"llvm-readelf failed for binary {binary} with output {process.stdout}"
            )
            raise Exception(f"Unreadable build-id for binary {binary}")
        data = json.loads(process.stdout)
        if len(data) != 1:
            raise Exception(
                f"Unreadable output from llvm-readelf for binary {binary}"
            )
        notes = data[0]["Notes"]
        for note in notes:
            note_section = note["NoteSection"]
            if note_section["Name"] == ".note.gnu.build-id":
                return note_section["Note"]["Build ID"]
        raise Exception(f"Build ID not found for binary {binary}")

    def generate_buildid_dir(
        self,
        binary: Path,
        build_id_dir: Path,
        build_id: str,
        log_handler: logging.Logger,
    ):
        os.makedirs(build_id_dir, exist_ok=True)
        # TODO: change suffix to something else for binaries without debug
        # info.
        suffix = ".debug"
        # Hardlink the original binary
        build_id_prefix_dir = build_id_dir.joinpath(build_id[:2])
        unstripped_binary = build_id_prefix_dir.joinpath(build_id[2:] + suffix)
        build_id_prefix_dir.mkdir(parents=True, exist_ok=True)
        atomic_link(unstripped_binary, binary)
        assert unstripped_binary.exists()
        stripped_binary = unstripped_binary.with_suffix("")
        llvm_objcopy = Path(self.toolchain_dir).joinpath("bin", "llvm-objcopy")
        # TODO: Verify shared libs used in unit tests can be stripped
        # with "--strip-sections".
        strip_mode = "--strip-sections"
        check_call_with_logging(
            [
                llvm_objcopy,
                strip_mode,
                unstripped_binary,
                stripped_binary,
            ],
            stdout_handler=log_handler.info,
            stderr_handler=log_handler.error,
        )
        return stripped_binary

    def write_to_file(self):
        with open(self.env_file_path(), "w", encoding="utf-8") as f:
            json.dump(self.__dict__, f)

    def setup_logging(self, log_to_file=False):
        fs = logging.Formatter("%(levelname)s:%(name)s:%(message)s")
        if log_to_file:
            logfile_handler = logging.FileHandler(
                self.tmp_dir().joinpath("log")
            )
            logfile_handler.setLevel(logging.DEBUG)
            logfile_handler.setFormatter(fs)
            logging.getLogger().addHandler(logfile_handler)
        stream_handler = logging.StreamHandler(sys.stdout)
        stream_handler.setFormatter(fs)
        if self.verbose:
            stream_handler.setLevel(logging.DEBUG)
        else:
            stream_handler.setLevel(logging.INFO)
        logging.getLogger().addHandler(stream_handler)
        logging.getLogger().setLevel(logging.DEBUG)

    @property
    def package_server_log_path(self) -> Path:
        return self.tmp_dir().joinpath("package_server_log")

    @property
    def emulator_log_path(self) -> Path:
        return self.tmp_dir().joinpath("emulator_log")

    @property
    def packages_dir(self) -> Path:
        return self.tmp_dir().joinpath("packages")

    @property
    def output_dir(self) -> Path:
        return self.tmp_dir().joinpath("output")

    def read_sdk_version(self):
        meta_json_path = Path(self.sdk_dir).joinpath("meta", "manifest.json")
        with open(meta_json_path, encoding="utf-8") as f:
            meta_json = json.load(f)
            return meta_json["id"]

    TEST_REPO_NAME: ClassVar[str] = "llvm-testing"

    def repo_dir(self) -> Path:
        return self.tmp_dir().joinpath(self.TEST_REPO_NAME)

    def sdk_arch(self):
        machine = platform.machine()
        if machine == "x86_64":
            return "x64"
        if machine == "arm":
            return "a64"
        raise Exception(f"Unrecognized host architecture {machine}")

    def tool_path(self, tool) -> Path:
        return Path(self.sdk_dir).joinpath("tools", self.sdk_arch(), tool)

    def host_arch_triple(self):
        machine = platform.machine()
        if machine == "x86_64":
            return "x86_64-unknown-linux-gnu"
        if machine == "arm":
            return "aarch64-unknown-linux-gnu"
        raise Exception(f"Unrecognized host architecture {machine}")

    @property
    def pm_lockfile_path(self):
        return self.tmp_dir().joinpath("pm.lock")

    @property
    def ffx_daemon_log_path(self):
        return self.tmp_dir().joinpath("ffx_daemon_log")

    @property
    def ffx_isolate_dir(self):
        return self.tmp_dir().joinpath("ffx_isolate")

    @property
    def home_dir(self):
        return self.tmp_dir().joinpath("user-home")

    def start_ffx_isolation(self):
        # Most of this is translated directly from ffx's isolate library
        os.mkdir(self.ffx_isolate_dir)
        os.mkdir(self.home_dir)

        ffx_path = self.tool_path("ffx")
        ffx_env = self.ffx_cmd_env()

        # Start ffx daemon
        # We want this to be a long-running process that persists after the script finishes
        # pylint: disable=consider-using-with
        with open(
            self.ffx_daemon_log_path, "w", encoding="utf-8"
        ) as ffx_daemon_log_file:
            subprocess.Popen(
                [
                    ffx_path,
                    "daemon",
                    "start",
                ],
                env=ffx_env,
                stdout=ffx_daemon_log_file,
                stderr=ffx_daemon_log_file,
            )

        # Disable analytics
        check_call_with_logging(
            [
                ffx_path,
                "config",
                "analytics",
                "disable",
            ],
            env=ffx_env,
            stdout_handler=self.subprocess_logger.debug,
            stderr_handler=self.subprocess_logger.debug,
        )

        # Set configs
        configs = {
            "log.enabled": "true",
            "test.is_isolated": "true",
            "test.experimental_structured_output": "true",
        }
        for key, value in configs.items():
            check_call_with_logging(
                [
                    ffx_path,
                    "config",
                    "set",
                    key,
                    value,
                ],
                env=ffx_env,
                stdout_handler=self.subprocess_logger.debug,
                stderr_handler=self.subprocess_logger.debug,
            )

    def ffx_cmd_env(self):
        return {
            "HOME": self.home_dir,
            "FFX_ISOLATE_DIR": self.ffx_isolate_dir,
            # We want to use our own specified temp directory
            "TMP": self.tmp_dir(),
            "TEMP": self.tmp_dir(),
            "TMPDIR": self.tmp_dir(),
            "TEMPDIR": self.tmp_dir(),
        }

    def stop_ffx_isolation(self):
        check_call_with_logging(
            [
                self.tool_path("ffx"),
                "daemon",
                "stop",
                #    "-w",
            ],
            env=self.ffx_cmd_env(),
            stdout_handler=self.subprocess_logger.debug,
            stderr_handler=self.subprocess_logger.debug,
        )

    def start(self):
        """Sets up the testing environment and prepares to run tests.

        Args:
            args: The command-line arguments to this command.

        During setup, this function will:
        - Locate necessary shared libraries
        - Create a new temp directory (this is where all temporary files are stored)
        - Start an emulator
        - Start an update server
        - Create a new package repo and register it with the emulator
        - Write test environment settings to a temporary file
        """

        # Initialize temp directory
        os.makedirs(self.tmp_dir(), exist_ok=True)
        if len(os.listdir(self.tmp_dir())) != 0:
            raise Exception(
                f"Temp directory is not clean (in {self.tmp_dir()})"
            )
        self.setup_logging(log_to_file=True)
        os.mkdir(self.output_dir)

        ffx_path = self.tool_path("ffx")
        ffx_env = self.ffx_cmd_env()

        # Start ffx isolation
        self.env_logger.info("Starting ffx isolation...")
        self.start_ffx_isolation()

        # Stop any running emulators (there shouldn't be any)
        check_call_with_logging(
            [
                ffx_path,
                "emu",
                "stop",
                "--all",
            ],
            env=ffx_env,
            stdout_handler=self.subprocess_logger.debug,
            stderr_handler=self.subprocess_logger.debug,
        )

        if not self.local_pb_path:
            self.local_pb_path = os.path.join(self.tmp_dir(), "local_pb")
        else:
            self.local_pb_path = os.path.abspath(self.local_pb_path)
        if not self.use_local_pb or not os.path.exists(self.local_pb_path):
            shutil.rmtree(self.local_pb_path, ignore_errors=True)
            self.env_logger.info("Download emulator image")
            product_bundle = "minimal." + self.triple_to_arch(self.target)
            sdk_version = self.read_sdk_version()
            output = subprocess.check_output(
                [
                    ffx_path,
                    "product",
                    "lookup",
                    product_bundle,
                    sdk_version,
                    "--base-url",
                    "gs://fuchsia/development/%s" % sdk_version,
                ],
                env=ffx_env,
            )
            outputs = str(output, encoding="utf-8").splitlines()
            gs_url = outputs[-1].strip()
            check_call_with_logging(
                [ffx_path, "product", "download", gs_url, self.local_pb_path],
                stdout_handler=self.subprocess_logger.debug,
                stderr_handler=self.subprocess_logger.debug,
            )
        else:
            self.env_logger.info(
                'Using existing emulator image at "%s"' % self.local_pb_path
            )

        # Start emulator
        self.env_logger.info("Starting emulator...")

        # FIXME: condition --accel hyper on target arch matching host arch
        check_call_with_logging(
            [
                ffx_path,
                "emu",
                "start",
                self.local_pb_path,
                "--headless",
                "--log",
                self.emulator_log_path,
                "--net",
                "auto",
                "--accel",
                "auto",
            ],
            env=ffx_env,
            stdout_handler=self.subprocess_logger.debug,
            stderr_handler=self.subprocess_logger.debug,
        )

        # Create new package repo
        self.env_logger.info("Creating package repo...")
        check_call_with_logging(
            [
                self.tool_path("pm"),
                "newrepo",
                "-repo",
                self.repo_dir(),
            ],
            stdout_handler=self.subprocess_logger.debug,
            stderr_handler=self.subprocess_logger.debug,
        )

        # Add repo
        check_call_with_logging(
            [
                ffx_path,
                "repository",
                "add-from-pm",
                self.repo_dir(),
                "--repository",
                self.TEST_REPO_NAME,
            ],
            env=ffx_env,
            stdout_handler=self.subprocess_logger.debug,
            stderr_handler=self.subprocess_logger.debug,
        )

        # Start repository server
        check_call_with_logging(
            [ffx_path, "repository", "server", "start", "--address", "[::]:0"],
            env=ffx_env,
            stdout_handler=self.subprocess_logger.debug,
            stderr_handler=self.subprocess_logger.debug,
        )

        # Register with newly-started emulator
        check_call_with_logging(
            [
                ffx_path,
                "target",
                "repository",
                "register",
                "--repository",
                self.TEST_REPO_NAME,
            ],
            env=ffx_env,
            stdout_handler=self.subprocess_logger.debug,
            stderr_handler=self.subprocess_logger.debug,
        )

        # Create lockfiles
        self.pm_lockfile_path.touch()

        # Write to file
        self.write_to_file()

        self.env_logger.info("Success! Your environment is ready to run tests.")

    # FIXME: shardify this
    # `facet` statement required for TCP testing via
    # protocol `fuchsia.posix.socket.Provider`. See
    # https://fuchsia.dev/fuchsia-src/development/testing/components/test_runner_framework?hl=en#legacy_non-hermetic_tests
    CML_TEMPLATE: ClassVar[
        str
    ] = """
    {{
        program: {{
            runner: "elf_test_runner",
            binary: "bin/{exe_name}",
            forward_stderr_to: "log",
            forward_stdout_to: "log",
            environ: [{env_vars}
            ]
        }},
        capabilities: [
            {{ protocol: "fuchsia.test.Suite" }},
        ],
        expose: [
            {{
                protocol: "fuchsia.test.Suite",
                from: "self",
            }},
        ],
        use: [
            {{ storage: "data", path: "/data" }},
            {{ storage: "tmp", path: "/tmp" }},
            {{ storage: "custom_artifacts", rights: [ "rw*" ], path: "/custom_artifacts" }},
            {{ protocol: [ "fuchsia.process.Launcher" ] }},
            {{ protocol: [ "fuchsia.posix.socket.Provider" ] }}
        ],
        facets: {{
            "fuchsia.test": {{ type: "system" }},
        }},
    }}
    """

    MANIFEST_TEMPLATE = """
    meta/package={package_dir}/meta/package
    meta/{package_name}.cm={package_dir}/meta/{package_name}.cm
    meta/fuchsia.abi/abi-revision={package_dir}/meta/abi-revision
    bin/{exe_name}={bin_path}
    lib/libc++.so.2={libcxx_path}
    lib/libc++abi.so.1={libcxx_abi_path}
    lib/libunwind.so.1={libunwind_path}
    lib/ld.so.1={sdk_dir}/arch/{target_arch}/sysroot/dist/lib/ld.so.1
    lib/libfdio.so={sdk_dir}/arch/{target_arch}/dist/libfdio.so
    """

    # TODO: Change it to LLVM Env Vars
    TEST_ENV_VARS: ClassVar[List[str]] = [
        "GTEST_",
    ]

    def run(self, args):
        """Runs the requested test in the testing environment.

        Args:
        args: The command-line arguments to this command.
        Returns:
        The return code of the test (0 for success, else failure).

        To run a test, this function will:
        - Create, compile, archive, and publish a test package
        - Run the test package on the emulator
        - Forward the test's stdout and stderr as this script's stdout and stderr
        """

        bin_path = Path(args.bin_path).absolute()

        # Find libcxx and libcxx-abi
        # TODO: Support AArch64 and RISC V when needed.
        toolchain_dir = Path(self.toolchain_dir)
        libcxx_path = toolchain_dir.joinpath(
            "lib", "x86_64-unknown-fuchsia", "libc++.so.2.0"
        )

        libcxx_abi_path = toolchain_dir.joinpath(
            "lib",
            "x86_64-unknown-fuchsia",
            "libc++abi.so.1.0",
        )
        libunwind_path = toolchain_dir.joinpath(
            "lib",
            "x86_64-unknown-fuchsia",
            "libunwind.so.1.0",
        )

        # Find shared libraries built with the unit test
        bin_parent_path = bin_path.parent
        shared_libs_paths = glob.glob(os.path.join(bin_parent_path, "*.so"))

        base_name = os.path.basename(os.path.dirname(args.bin_path))
        exe_name = base_name.lower().replace(".", "_")
        build_id = self.build_id(bin_path)
        package_name = f"{exe_name}_{build_id}"

        package_dir = self.packages_dir.joinpath(package_name)
        cml_path = package_dir.joinpath("meta", f"{package_name}.cml")
        cm_path = package_dir.joinpath("meta", f"{package_name}.cm")
        abi_revision_path = package_dir.joinpath("meta", "abi-revision")
        manifest_path = package_dir.joinpath(f"{package_name}.manifest")
        far_path = package_dir.joinpath(f"{package_name}-0.far")
        gtest_json_path = ""

        arguments = args.arguments

        test_output_dir = self.output_dir.joinpath(package_name)

        # Clean and create temporary output directory
        if test_output_dir.exists():
            shutil.rmtree(test_output_dir)
        test_output_dir.mkdir(parents=True)

        # Open log file
        runner_logger = logging.getLogger(f"env.package.{package_name}")
        runner_logger.setLevel(logging.DEBUG)
        logfile_handler = logging.FileHandler(test_output_dir.joinpath("log"))
        logfile_handler.setLevel(logging.DEBUG)
        logfile_handler.setFormatter(
            logging.Formatter("%(levelname)s:%(name)s:%(message)s")
        )
        runner_logger.addHandler(logfile_handler)

        # TODO: setup output file and stdout
        runner_logger.info(f"Bin path: {bin_path}")
        runner_logger.info("Setting up package...")

        # Link binary to build-id dir and strip it.
        build_id_dir = self.tmp_dir().joinpath(".build-id")
        stripped_binary = self.generate_buildid_dir(
            binary=bin_path,
            build_id_dir=build_id_dir,
            build_id=build_id,
            log_handler=runner_logger,
        )
        runner_logger.info(f"Stripped Bin path: {stripped_binary}")

        # Set up package
        check_call_with_logging(
            [
                self.tool_path("pm"),
                "-o",
                package_dir,
                "-n",
                package_name,
                "init",
            ],
            stdout_handler=runner_logger.info,
            stderr_handler=runner_logger.warning,
        )

        runner_logger.info("Writing CML...")

        # Write and compile CML
        with open(cml_path, "w", encoding="utf-8") as cml:
            # Collect environment variables
            env_vars = ""
            for var_name in os.environ:
                if var_name == "GTEST_OUTPUT":
                    var_value = os.getenv(var_name)
                    if var_value is None:
                        continue
                    var_value = var_value.split(":")
                    gtest_json_path = var_value[1]
                    var_value = "%s:%s" % (
                        var_value[0],
                        "/custom_artifacts/output.json",
                    )
                    env_vars += f'\n            "{var_name}={var_value}",'
                else:
                    for test_env_name in self.TEST_ENV_VARS:
                        if var_name.startswith(test_env_name):
                            var_value = os.getenv(var_name)
                            if var_value is not None:
                                env_vars += (
                                    f'\n            "{var_name}={var_value}",'
                                )
            cml.write(
                self.CML_TEMPLATE.format(env_vars=env_vars, exe_name=exe_name)
            )

        runner_logger.info("Compiling CML...")

        check_call_with_logging(
            [
                self.tool_path("cmc"),
                "compile",
                cml_path,
                "--includepath",
                ".",
                "--output",
                cm_path,
            ],
            stdout_handler=runner_logger.info,
            stderr_handler=runner_logger.warning,
        )

        runner_logger.info("Write abi-revision")
        with open(abi_revision_path, "wb") as f:
            revision = int(self.abi_revision, 16)
            f.write(struct.pack("<Q", revision))

        runner_logger.info("Writing manifest...")

        # Write, build, and archive manifest
        with open(manifest_path, "w", encoding="utf-8") as manifest:
            manifest.write(
                self.MANIFEST_TEMPLATE.format(
                    bin_path=stripped_binary,
                    exe_name=exe_name,
                    package_dir=package_dir,
                    package_name=package_name,
                    libcxx_path=libcxx_path,
                    libcxx_abi_path=libcxx_abi_path,
                    libunwind_path=libunwind_path,
                    target=self.target,
                    sdk_dir=self.sdk_dir,
                    target_arch=self.triple_to_arch(self.target),
                )
            )
            for shared_lib in shared_libs_paths:
                shared_lib_build_id = self.build_id(shared_lib)
                stripped_shared_lib = self.generate_buildid_dir(
                    binary=shared_lib,
                    build_id_dir=build_id_dir,
                    build_id=shared_lib_build_id,
                    log_handler=runner_logger,
                )
                manifest.write(
                    f"lib/{os.path.basename(shared_lib)}={stripped_shared_lib}\n"
                )
        runner_logger.info("Compiling and archiving manifest...")

        check_call_with_logging(
            [
                self.tool_path("pm"),
                "-o",
                package_dir,
                "-m",
                manifest_path,
                "build",
            ],
            stdout_handler=runner_logger.info,
            stderr_handler=runner_logger.warning,
        )
        check_call_with_logging(
            [
                self.tool_path("pm"),
                "-o",
                package_dir,
                "-m",
                manifest_path,
                "archive",
            ],
            stdout_handler=runner_logger.info,
            stderr_handler=runner_logger.warning,
        )

        runner_logger.info("Publishing package to repo...")

        # Publish package to repo
        with open(self.pm_lockfile_path, "w") as pm_lockfile:
            fcntl.lockf(pm_lockfile.fileno(), fcntl.LOCK_EX)
            check_call_with_logging(
                [
                    self.tool_path("pm"),
                    "publish",
                    "-a",
                    "-repo",
                    self.repo_dir(),
                    "-f",
                    far_path,
                ],
                stdout_handler=runner_logger.info,
                stderr_handler=runner_logger.warning,
            )
            # This lock should be released automatically when the pm
            # lockfile is closed, but we'll be polite and unlock it now
            # since the spec leaves some wiggle room.
            fcntl.lockf(pm_lockfile.fileno(), fcntl.LOCK_UN)

        runner_logger.info("Running ffx test...")
        # Rewrite the gtest_output path to the actual output directory.
        for i in range(len(arguments)):
            arg_value = arguments[i]
            if "--gtest_output" == arg_value:
                if (i + 1) >= len(arguments):
                    runner_logger.error(
                        "Unexpected '--gtest_output' argument in %s"
                        % str(arguments)
                    )
                    self.env_logger.error(
                        "Unexpected '--gtest_output' argument"
                    )
                    return 1
                path_value = arguments[i + 1]
                path_value = path_value.split(":")
                gtest_json_path = path_value[1]
                path_value = "%s:%s" % (
                    path_value[0],
                    "/custom_artifacts/output.json",
                )
                arguments[i + 1] = path_value
                continue
            elif "--gtest_output=" in arg_value:
                path_value = arg_value[len("--gtest_output=") :]
                path_value = path_value.split(":")
                gtest_json_path = path_value[1]
                path_value = "%s:%s" % (
                    path_value[0],
                    "/custom_artifacts/output.json",
                )
                arguments[i] = "--gtest_output=%s" % path_value
                continue

        # Run test on emulator
        check_call_with_logging(
            [
                self.tool_path("ffx"),
                "test",
                "run",
                f"fuchsia-pkg://{self.TEST_REPO_NAME}/{package_name}#meta/{package_name}.cm",
                "--min-severity-logs",
                "TRACE",
                "--output-directory",
                test_output_dir,
                "--",
            ]
            + arguments,
            env=self.ffx_cmd_env(),
            check=False,
            stdout_handler=runner_logger.info,
            stderr_handler=runner_logger.warning,
        )

        if gtest_json_path:
            output_json_paths = glob.glob(
                os.path.join(test_output_dir, "**", "output.json"),
                recursive=True,
            )
            if len(output_json_paths) == 0:
                runner_logger.error(
                    f"{test_output_dir} contains no output JSON file"
                )
            else:
                if len(output_json_paths) > 1:
                    runner_logger.error(
                        f"{test_output_dir} contains more than one output JSON"
                    )
                shutil.copyfile(output_json_paths[0], gtest_json_path)

        runner_logger.info("Reporting test suite output...")

        # Read test suite output
        run_summary_path = test_output_dir.joinpath("run_summary.json")
        if not run_summary_path.exists():
            runner_logger.error("Failed to open test run summary")
            return 254

        with open(run_summary_path, encoding="utf-8") as f:
            run_summary = json.load(f)

        suite = run_summary["data"]["suites"][0]
        case = suite["cases"][0]

        return_code = 0 if case["outcome"] == "PASSED" else 1

        artifacts = case["artifacts"]
        artifact_dir = case["artifact_dir"]
        stdout_path = None
        stderr_path = None

        for path, artifact in artifacts.items():
            artifact_path = os.path.join(test_output_dir, artifact_dir, path)
            artifact_type = artifact["artifact_type"]

            if artifact_type == "STDERR":
                stderr_path = artifact_path
            elif artifact_type == "STDOUT":
                stdout_path = artifact_path

        if stdout_path is not None:
            if not os.path.exists(stdout_path):
                runner_logger.error(
                    f"stdout file {stdout_path} does not exist."
                )
            else:
                with open(stdout_path, encoding="utf-8", errors="ignore") as f:
                    runner_logger.info(f.read())
        if stderr_path is not None:
            if not os.path.exists(stderr_path):
                runner_logger.error(
                    f"stderr file {stderr_path} does not exist."
                )
            else:
                with open(stderr_path, encoding="utf-8", errors="ignore") as f:
                    runner_logger.error(f.read())

        runner_logger.info("Done!")
        return return_code

    def stop(self):
        """Shuts down and cleans up the testing environment.

        Args:
        args: The command-line arguments to this command.
        Returns:
        The return code of the test (0 for success, else failure).

        During cleanup, this function will stop the emulator, package server, and
        update server, then delete all temporary files. If an error is encountered
        while stopping any running processes, the temporary files will not be deleted.
        """

        self.env_logger.debug("Reporting logs...")

        # Print test log files
        for test_dir in os.listdir(self.output_dir):
            log_path = os.path.join(self.output_dir, test_dir, "log")
            self.env_logger.debug(f"\n---- Logs for test '{test_dir}' ----\n")
            if os.path.exists(log_path):
                with open(log_path, encoding="utf-8", errors="ignore") as log:
                    self.env_logger.debug(log.read())
            else:
                self.env_logger.debug("No logs found")

        # Print the emulator log
        self.env_logger.debug("\n---- Emulator logs ----\n")
        if os.path.exists(self.emulator_log_path):
            with open(self.emulator_log_path, encoding="utf-8") as log:
                self.env_logger.debug(log.read())
        else:
            self.env_logger.debug("No emulator logs found")

        # Print the package server log
        self.env_logger.debug("\n---- Package server log ----\n")
        if os.path.exists(self.package_server_log_path):
            with open(self.package_server_log_path, encoding="utf-8") as log:
                self.env_logger.debug(log.read())
        else:
            self.env_logger.debug("No package server log found")

        # Print the ffx daemon log
        self.env_logger.debug("\n---- ffx daemon log ----\n")
        if os.path.exists(self.ffx_daemon_log_path):
            with open(self.ffx_daemon_log_path, encoding="utf-8") as log:
                self.env_logger.debug(log.read())
        else:
            self.env_logger.debug("No ffx daemon log found")

        # Shut down the emulator
        self.env_logger.info("Stopping emulator...")
        check_call_with_logging(
            [
                self.tool_path("ffx"),
                "emu",
                "stop",
            ],
            env=self.ffx_cmd_env(),
            stdout_handler=self.subprocess_logger.debug,
            stderr_handler=self.subprocess_logger.debug,
        )

        # Stop ffx isolation
        self.env_logger.info("Stopping ffx isolation...")
        self.stop_ffx_isolation()

    def cleanup(self):
        # Remove temporary files
        self.env_logger.info("Deleting temporary files...")
        shutil.rmtree(self.tmp_dir(), ignore_errors=True)

    def syslog(self, args):
        subprocess.run(
            [
                self.tool_path("ffx"),
                "log",
                "--since",
                "now",
            ],
            env=self.ffx_cmd_env(),
            check=False,
        )


def start(args):
    test_env = TestEnvironment.from_args(args)
    test_env.start()
    return 0


def run(args):
    test_env = TestEnvironment.read_from_file()
    test_env.setup_logging(log_to_file=True)
    return test_env.run(args)


def stop(args):
    test_env = TestEnvironment.read_from_file()
    test_env.setup_logging(log_to_file=False)
    test_env.stop()
    if not args.no_cleanup:
        test_env.cleanup()
    return 0


def cleanup(args):
    del args
    test_env = TestEnvironment.read_from_file()
    test_env.setup_logging(log_to_file=False)
    test_env.cleanup()
    return 0


def syslog(args):
    test_env = TestEnvironment.read_from_file()
    test_env.setup_logging(log_to_file=True)
    test_env.syslog(args)
    return 0


def main():
    parser = argparse.ArgumentParser()

    subparsers = parser.add_subparsers()

    start_parser = subparsers.add_parser(
        "start", help="initializes the testing environment"
    )
    start_parser.add_argument(
        "--build-dir",
        help="the current compiler build directory",
        required=True,
    )
    start_parser.add_argument(
        "--sdk",
        help="the directory of the fuchsia SDK",
        required=True,
    )
    start_parser.add_argument(
        "--verbose",
        help="prints more output from executed processes",
        action="store_true",
    )
    start_parser.add_argument(
        "--target",
        help="the target platform to test",
        required=True,
    )
    start_parser.add_argument(
        "--toolchain-dir",
        help="the toolchain directory",
        required=True,
    )
    start_parser.add_argument(
        "--abi-revision",
        help="the 64-bit abi revision in hex string",
        default="0x099D5AB9C26B64DA",
    )
    start_parser.add_argument(
        "--local-product-bundle-path",
        help="the path where the product-bundle should be downloaded to",
    )
    start_parser.add_argument(
        "--use-local-product-bundle-if-exists",
        help="if the product bundle already exists in the local path, use it instead of downloading it again",
        action="store_true",
    )
    start_parser.set_defaults(func=start)

    run_parser = subparsers.add_parser(
        "run", help="run a test in the testing environment"
    )
    run_parser.add_argument("bin_path", help="path to the binary to run")
    run_parser.add_argument(
        "arguments",
        help="the shared libs passed along with the binary",
        nargs=argparse.REMAINDER,
    )
    run_parser.set_defaults(func=run)

    stop_parser = subparsers.add_parser(
        "stop", help="shuts down and cleans up the testing environment"
    )
    stop_parser.add_argument(
        "--no-cleanup",
        default=False,
        action="store_true",
        help="don't delete temporary files after stopping",
    )
    stop_parser.set_defaults(func=stop)

    cleanup_parser = subparsers.add_parser(
        "cleanup",
        help="deletes temporary files after the testing environment has been manually cleaned up",
    )
    cleanup_parser.set_defaults(func=cleanup)

    syslog_parser = subparsers.add_parser(
        "syslog", help="prints the device syslog"
    )
    syslog_parser.set_defaults(func=syslog)

    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
