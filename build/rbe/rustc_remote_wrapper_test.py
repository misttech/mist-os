#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests for rustc_remote_wrapper."""

import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any, Collection, Iterable, Sequence
from unittest import mock

import cl_utils
import fuchsia
import remote_action
import rustc
import rustc_remote_wrapper


class ImmediateExit(Exception):
    """For mocking functions that do not return."""


def _write_file_contents(path: Path, contents: str) -> None:
    with open(path, "w") as f:
        f.write(contents)


def _read_file_contents(path: Path) -> str:
    with open(path, "r") as f:
        return f.read()


def _strs(items: Sequence[Any]) -> Sequence[str]:
    return [str(i) for i in items]


def _paths(items: Sequence[Any]) -> Collection[Path]:
    if isinstance(items, list):
        return [Path(i) for i in items]
    elif isinstance(items, set):
        return {Path(i) for i in items}
    elif isinstance(items, tuple):
        return tuple(Path(i) for i in items)

    t = type(items)
    raise TypeError(f"Unhandled sequence type: {t}")


class AccompanyRlibWithSoTests(unittest.TestCase):
    def test_not_rlib(self) -> None:
        deps = Path("../path/to/foo.rs")
        self.assertEqual(
            list(rustc_remote_wrapper.accompany_rlib_with_so([deps])), [deps]
        )

    def test_rlib_without_so(self) -> None:
        deps = Path("obj/build/foo.rlib")
        with mock.patch.object(
            Path, "is_file", return_value=False
        ) as mock_isfile:
            self.assertEqual(
                list(rustc_remote_wrapper.accompany_rlib_with_so([deps])),
                [deps],
            )
        mock_isfile.assert_called_once()

    def test_rlib_with_so(self) -> None:
        deps = Path("obj/build/foo.rlib")
        with mock.patch.object(
            Path, "is_file", return_value=True
        ) as mock_isfile:
            self.assertEqual(
                list(rustc_remote_wrapper.accompany_rlib_with_so([deps])),
                [deps, Path("obj/build/foo.so")],
            )
            mock_isfile.assert_called_once()


class ExpandDepsForRlibCompileTests(unittest.TestCase):
    def test_proc_macro(self) -> None:
        deps = list(
            rustc_remote_wrapper.expand_deps_for_rlib_compile(
                [Path("foo/barmac.so")]
            )
        )
        self.assertEqual(deps, [Path("foo/barmac.so")])

    def test_rlib(self) -> None:
        deps = list(
            rustc_remote_wrapper.expand_deps_for_rlib_compile(
                [Path("foo/bar.rlib")]
            )
        )
        self.assertEqual(deps, [Path("foo/bar.rlib")])

    def test_rmeta_missing_rlib(self) -> None:
        deps = list(
            rustc_remote_wrapper.expand_deps_for_rlib_compile(
                [Path("foo/bar.rmeta")]
            )
        )
        self.assertEqual(deps, [Path("foo/bar.rmeta")])

    def test_rmeta_with_coexisting_rlib(self) -> None:
        with mock.patch.object(
            Path, "exists", return_value=True
        ) as mock_rlib_exists:
            deps = list(
                rustc_remote_wrapper.expand_deps_for_rlib_compile(
                    [Path("foo/bar.rmeta")]
                )
            )
        mock_rlib_exists.assert_called_once_with()
        self.assertEqual(deps, [Path("foo/bar.rmeta"), Path("foo/bar.rlib")])


class RustRemoteActionPrepareTests(unittest.TestCase):
    def generate_prepare_mocks(
        self,
        depfile_contents: Sequence[str] | None = None,
        compiler_shlibs: Sequence[Path] | None = None,
        clang_rt_libdir: Path | None = None,
        libcxx_static: Path | None = None,
    ) -> Iterable[Any]:
        """common mocks needed for RustRemoteAction.prepare()"""
        # depfile only command
        yield mock.patch.object(
            rustc_remote_wrapper, "_make_local_depfile", return_value=0
        )

        if depfile_contents:
            yield mock.patch.object(
                rustc_remote_wrapper,
                "_readlines_from_file",
                return_value=[line + "\n" for line in depfile_contents],
            )

        # check compiler is executable
        yield mock.patch.object(
            rustc_remote_wrapper, "_tool_is_executable", return_value=True
        )

        if compiler_shlibs:
            yield mock.patch.object(
                rustc_remote_wrapper.RustRemoteAction,
                "_remote_rustc_shlibs",
                return_value=iter(compiler_shlibs),
            )

        yield mock.patch.object(
            rustc_remote_wrapper, "_libcxx_isfile", return_value=True
        )

        if clang_rt_libdir:
            yield mock.patch.object(
                fuchsia, "clang_runtime_libdirs", return_value=[clang_rt_libdir]
            )

        if libcxx_static:
            yield mock.patch.object(
                fuchsia, "clang_libcxx_static", return_value=libcxx_static or ""
            )

        # expected to be called through _rust_stdlib_libunwind_inputs()
        # when --sysroot is unspecified.
        yield mock.patch.object(
            rustc.RustAction,
            "default_rust_sysroot",
            return_value=Path("../some/random/sysroot"),
        )

    def test_prepare_basic(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_contents = [str(d) + ":" for d in deps]
        command = _strs([compiler, source, "-o", rlib])
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        self.assertIsNone(r.clang_cxx_stdlibdir)
        dep_command, aux_rspfiles = r.dep_only_command_with_rspfiles
        (
            internal_dep_command,
            internal_aux_rspfiles,
        ) = r._rust_action.dep_only_command_with_rspfiles(str(r.local_depfile))
        self.assertEqual(dep_command, internal_dep_command)
        self.assertEqual(aux_rspfiles, internal_aux_rspfiles)
        self.assertEqual(aux_rspfiles, [])

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps)
        )
        self.assertEqual(remote_output_files, {rlib})

    def test_cxx_stdlibdir(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_contents = [str(d) + ":" for d in deps]
        command = _strs([compiler, source, "-o", rlib])
        cxx_libdir = Path("../tools/clang/find/libcxx/here")
        r = rustc_remote_wrapper.RustRemoteAction(
            [f"--cxx-stdlibdir={cxx_libdir}", "--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        self.assertEqual(r.clang_cxx_stdlibdir, cxx_libdir)

    def test_prepare_with_rmeta_file_in_scanned_deps(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        rmeta_dep = Path("obj/bar.rmeta")
        implied_rlib_dep = Path("obj/bar.rlib")
        deps = [rmeta_dep]
        depfile_contents = [str(d) + ":" for d in deps]

        with tempfile.TemporaryDirectory() as td:
            command = _strs([compiler, source, "-o", rlib])
            r = rustc_remote_wrapper.RustRemoteAction(
                ["--", *command],
                exec_root=exec_root,
                working_dir=working_dir,
                auto_reproxy=False,
            )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            # 'expand_rmetas' checks for file existence
            # specificially co-existence of .rlib with .rmeta
            with mock.patch.object(
                Path, "exists", return_value=True
            ) as mock_rlib_exists:
                prepare_status = r.prepare()

            mock_rlib_exists.assert_called()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        # The corresponding .rlib from the .rmeta dep is considered a remote input.
        self.assertEqual(
            remote_inputs,
            set([compiler, shlib_rel, source, implied_rlib_dep] + deps),
        )
        self.assertEqual(remote_output_files, {rlib})

    def test_prepare_with_response_file(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_contents = [str(d) + ":" for d in deps]

        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            rsp = tdp / "foo.rsp"
            rsp.write_text(
                "\n".join(_strs(["# expand these args", source, "-o", rlib]))
                + "\n"
            )
            command = _strs(
                [compiler, f"@{rsp}"]
            )  # pass args through response file
            r = rustc_remote_wrapper.RustRemoteAction(
                ["--", *command],
                exec_root=exec_root,
                working_dir=working_dir,
                auto_reproxy=False,
            )

            mocks = self.generate_prepare_mocks(
                depfile_contents=depfile_contents,
                compiler_shlibs=[shlib_rel],
            )
            with contextlib.ExitStack() as stack:
                for m in mocks:
                    stack.enter_context(m)
                with mock.patch.object(
                    rustc_remote_wrapper, "_remove_files"
                ) as mock_remove:
                    prepare_status = r.prepare()

            self.assertEqual(prepare_status, 0)  # success

            # Expect there was a temporary response file written next to the
            # original one, also inside the temporary dir.
            # It should be marked for cleanup.
            mock_remove.assert_called_once()
            args, _ = mock_remove.call_args_list[0]
            self.assertEqual(len(args), 1)
            # args[0] is the list of files to cleanup
            self.assertGreater(len(args[0]), 0)
            self.assertTrue(any(str(f).startswith(str(rsp)) for f in args[0]))

        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source, rsp] + deps)
        )
        self.assertEqual(remote_output_files, {rlib})

    def test_dep_only_command_filtered(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_path = Path("obj/foo.rlib.d")
        depfile_contents = [str(d) + ":" for d in deps]
        command = _strs(
            [
                compiler,
                source,
                "--local-only=--foo=bar",
                "-o",
                rlib,
                f"--emit=dep-info={depfile_path}",
            ]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )
        dep_command, _ = r.dep_only_command_with_rspfiles
        self.assertIn("--foo=bar", dep_command)
        self.assertNotIn("--local-only=--foo=bar", dep_command)

    def test_prepare_depfile(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_path = Path("obj/foo.rlib.d")
        depfile_contents = [str(d) + ":" for d in deps]
        command = _strs(
            [compiler, source, "-o", rlib, f"--emit=dep-info={depfile_path}"]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps)
        )
        self.assertEqual(remote_output_files, {rlib, depfile_path})
        self.assertFalse(a.skipping_some_download)
        # Even if download_outputs=false, the depfile is required locally.
        self.assertEqual(set(a.expected_downloads), remote_output_files)

    def test_prepare_depfile_download_false(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_path = Path("obj/foo.rlib.d")
        depfile_contents = [str(d) + ":" for d in deps]
        # skip downloading the main output
        command = _strs(
            [
                compiler,
                source,
                "-o",
                rlib,
                f"--emit=dep-info={depfile_path}",
                "--remote-flag=--download_outputs=false",
            ]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps)
        )
        self.assertEqual(remote_output_files, {rlib, depfile_path})
        self.assertTrue(a.skipping_some_download)
        # Even if download_outputs=false, the depfile is required locally.
        self.assertEqual(set(a.expected_downloads), {depfile_path})

    def test_prepare_depfile_failure(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        depfile_path = Path("obj/foo.rlib.d")
        # mocking failure, no need for actual depfile contents
        command = _strs(
            [compiler, source, "-o", rlib, f"--emit=dep-info={depfile_path}"]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        # Make sure internal RuntimeError gets handled.
        with mock.patch.object(
            rustc_remote_wrapper, "_make_local_depfile", return_value=1
        ) as mock_deps:
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 1)  # failure

    def test_prepare_llvm_ir(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        llvm_ir = Path("obj/foo.ll")
        deps = [Path("../foo/src/other.rs")]
        depfile_contents = [str(d) + ":" for d in deps]
        command = _strs([compiler, source, "-o", rlib, f"--emit=llvm-ir"])
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps)
        )
        # Even if download_outputs=false, we want the non-primary outputs.
        self.assertEqual(remote_output_files, {rlib, llvm_ir})
        self.assertFalse(a.skipping_some_download)
        self.assertEqual(set(a.expected_downloads), remote_output_files)

    def test_prepare_llvm_ir_download_false(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        llvm_ir = Path("obj/foo.ll")
        deps = [Path("../foo/src/other.rs")]
        depfile_contents = [str(d) + ":" for d in deps]
        command = _strs(
            [
                compiler,
                source,
                "-o",
                rlib,
                f"--emit=llvm-ir",
                "--remote-flag=--download_outputs=false",
            ]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps)
        )
        # Even if download_outputs=false, we want the non-primary outputs.
        self.assertEqual(remote_output_files, {rlib, llvm_ir})
        self.assertTrue(a.skipping_some_download)
        self.assertEqual(set(a.expected_downloads), {llvm_ir})

    def test_prepare_externs(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_contents = [str(d) + ":" for d in deps]
        externs = list(
            _paths(
                [
                    "obj/path/to/this.rlib",
                    "obj/path/to/that.rlib",
                ]
            )
        )
        extern_flags = [
            "--extern",
            f"this={externs[0]}",
            "--extern",
            f"that={externs[1]}",
        ]
        command: Sequence[str] = [
            *_strs([compiler, source, "-o", rlib]),
            *extern_flags,
        ]
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps + externs)
        )
        self.assertEqual(remote_output_files, {rlib})

    def test_prepare_link_arg_files(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_contents = [str(d) + ":" for d in deps]
        link_arg = Path("obj/some/random.a")
        command = _strs(
            [compiler, source, "-o", rlib, f"-Clink-arg={link_arg}"]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs,
            set([compiler, shlib_rel, source] + deps + [link_arg]),
        )
        self.assertEqual(remote_output_files, {rlib})

    def test_prepare_needs_linker(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        exec_root_rel = cl_utils.relpath(exec_root, start=working_dir)
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        linker = Path("../tools/bin/linker")
        lld = Path("../tools/bin/ld.lld")
        clang_rt_libdir = Path("../fake/lib/clang/x86_64-unknown-linux")
        libcxx = Path("fake/clang/libc++.a")
        source = Path("../foo/src/lib.rs")
        target = "x86_64-unknown-linux-gnu"
        exe = Path("obj/foo.exe")
        deps = [Path("../foo/src/other.rs")]
        depfile_contents = [str(d) + ":" for d in deps]
        command = _strs(
            [
                compiler,
                source,
                "-o",
                exe,
                "--crate-type",
                "bin",
                f"-Clinker={linker}",
                f"--target={target}",
                f"-Clink-arg=-fuse-ld=lld",
            ]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )
        self.assertEqual(r.linker, linker)
        self.assertEqual(r.remote_ld_path, lld)

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
            clang_rt_libdir=clang_rt_libdir,
            libcxx_static=exec_root_rel / libcxx,
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs,
            set(
                _paths(
                    [compiler, shlib_rel, source]
                    + deps
                    + [linker, lld, clang_rt_libdir, exec_root_rel / libcxx]
                )
            ),
        )
        self.assertEqual(remote_output_files, {exe})

    def test_prepare_remote_option_forwarding(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_contents = [str(d) + ":" for d in deps]
        remote_flag = "--some_forwarded_rewrapper_flag=value"  # not real
        command = _strs(
            [compiler, source, "-o", rlib, f"--remote-flag={remote_flag}"]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps)
        )
        self.assertEqual(remote_output_files, {rlib})
        # Position matters, not just presence.
        # Could override the set of standard options.
        self.assertEqual(r.remote_options[-1], remote_flag)

    def test_prepare_environment_names_input_file(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_contents = [str(d) + ":" for d in deps]
        env_file = Path("../path/to/config.json")
        command = _strs(
            [
                f"CONFIGURE_THINGY={env_file}",
                compiler,
                source,
                "-o",
                rlib,
                f"--remote-inputs={env_file}",
            ]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            # We are not scanning the environment variables automatically.
            # Users should follow the above example command and
            # explicitly pass --remote-inputs.
            with mock.patch.object(
                rustc_remote_wrapper, "_env_file_exists", return_value=True
            ) as mock_exists:
                prepare_status = r.prepare()
            mock_exists.assert_not_called()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs,
            set(_paths([compiler, shlib_rel, source, env_file] + deps)),
        )
        self.assertEqual(remote_output_files, {rlib})

    def test_native_compile(self) -> None:
        host_platform = fuchsia.REMOTE_PLATFORM
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path(f"../tools/{host_platform}/bin/rustc")
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        command = _strs([compiler, source, "-o", rlib])
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            host_platform=host_platform,
            auto_reproxy=False,
        )
        self.assertTrue(r.host_matches_remote)

    def test_target_linker_prefix(self) -> None:
        for target, ld in [
            ("arm64-unknown-linux-gnu", "ld"),
            ("x86_64-unknown-linux-gnu", "ld"),
            ("x86_64-unknown-freebsd", "ld"),
            ("x86_64-apple-darwin", "ld64"),
            ("ppc64-apple-darwin", "ld64"),
        ]:
            exec_root = Path("/home/project")
            working_dir = exec_root / "build-here"
            compiler = Path("../tools/host-platform/bin/rustc")
            source = Path("../foo/src/lib.rs")
            rlib = Path("obj/foo.rlib")
            command = _strs(
                [compiler, source, "-o", rlib, f"--target={target}"]
            )
            r = rustc_remote_wrapper.RustRemoteAction(
                ["--", *command],
                exec_root=exec_root,
                working_dir=working_dir,
                auto_reproxy=False,
            )
            self.assertEqual(r.target_linker_prefix, ld)

    def test_remote_mode_ok_with_relative_c_sysroot(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/host-platform/bin/rustc")
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        c_sysroot = Path("generated/sys/root")  # relative
        target = "aarch64-unknown-linux-gnu"
        command = _strs(
            [
                compiler,
                source,
                "-o",
                rlib,
                "--crate-type=bin",
                f"--target={target}",
                f"-Clink-arg=--sysroot={c_sysroot}",
            ]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )
        self.assertEqual(r.c_sysroot, c_sysroot)
        self.assertFalse(r._c_sysroot_is_outside_exec_root)
        self.assertTrue(r.needs_linker)
        self.assertFalse(r._remote_disqualified())
        self.assertFalse(r.local_only)
        sysroot_files = list(r._c_sysroot_files())
        self.assertNotEqual(sysroot_files, [])

    def test_forced_local_mode_ok_with_external_absolute_c_sysroot(
        self,
    ) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/host-platform/bin/rustc")
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        c_sysroot = Path("/fancy/pants/local/SDK")  # absolute, external
        target = "aarch64-unknown-linux-gnu"
        command = _strs(
            [
                compiler,
                source,
                "-o",
                rlib,
                "--crate-type=bin",
                f"--target={target}",
                f"-Clink-arg=--sysroot={c_sysroot}",
            ]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )
        self.assertEqual(r.c_sysroot, c_sysroot)
        self.assertTrue(r._c_sysroot_is_outside_exec_root)
        self.assertTrue(r.needs_linker)
        self.assertTrue(r._remote_disqualified())
        self.assertTrue(r.local_only)
        with mock.patch.object(fuchsia, "c_sysroot_files") as mock_files:
            sysroot_files = list(r._c_sysroot_files())
        self.assertEqual(sysroot_files, [])
        mock_files.assert_not_called()

    def test_cdylib_rust_lld(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/mac-x64/bin/rustc")
        remote_compiler = Path("../tools/linux-x64/bin/rustc")
        remote_rust_lld = (
            remote_compiler.parent / fuchsia._REMOTE_RUST_LLD_RELPATH
        )
        source = Path("../foo/src/lib.rs")
        dylib = Path("obj/foo.dylib")
        command = _strs(
            [compiler, source, "--crate-type", "cdylib", "-o", dylib]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )
        mocks = self.generate_prepare_mocks()
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            with mock.patch.object(
                rustc_remote_wrapper.RustRemoteAction, "_local_depfile_inputs"
            ) as mock_deps:
                with mock.patch.object(
                    rustc_remote_wrapper.RustRemoteAction,
                    "_remote_rustc_shlibs",
                ) as mock_shlibs:
                    r.prepare()

        mock_deps.assert_called_once()
        mock_shlibs.assert_called_once()
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        self.assertIn(remote_compiler, remote_inputs)
        self.assertIn(remote_rust_lld, remote_inputs)

    def test_prepare_remote_cross_compile(self) -> None:
        host_platform = "mac-arm64"
        remote_platform = fuchsia.REMOTE_PLATFORM
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path(f"../tools/{host_platform}/bin/rustc")
        remote_compiler = Path(f"../tools/{remote_platform}/bin/rustc")
        linker = Path(f"../tools/{host_platform}/bin/linker")
        remote_linker = Path(f"../tools/{remote_platform}/bin/linker")
        remote_ld_path = Path(f"../tools/{remote_platform}/bin/ld.lld")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        libcxx = Path("../std/tools/bin/../lib/libc++.a")
        libcxx_norm = Path("../std/tools/lib/libc++.a")
        target = "powerpc-unknown-freebsd"
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_contents = [str(d) + ":" for d in deps]
        command = _strs(
            [
                compiler,
                source,
                "-o",
                rlib,
                f"--target={target}",
                f"-Clinker={linker}",
                f"-Clink-arg={libcxx}",
                f"-Clink-arg=-fuse-ld=lld",
            ]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            host_platform=host_platform,
            auto_reproxy=False,
        )
        self.assertEqual(r.working_dir, working_dir)
        self.assertFalse(r.host_matches_remote)
        self.assertEqual(r.original_command, command)
        self.assertEqual(r.linker, linker)
        self.assertEqual(r.remote_linker, remote_linker)
        self.assertEqual(r.remote_ld_path, remote_ld_path)

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()
            remote_command = list(r.remote_compile_command())

        self.assertEqual(prepare_status, 0)  # success
        self.assertEqual(r.remote_compiler, remote_compiler)
        a = r.remote_action
        self.assertEqual(
            remote_command,
            _strs(
                [
                    remote_compiler,
                    source,
                    "-o",
                    rlib,
                    f"--target={target}",
                    f"-Clinker={remote_linker}",
                    f"-Clink-arg={libcxx_norm}",  # normalized
                    f"-Clink-arg=-fuse-ld=lld",
                    f"-Clink-arg=--ld-path={remote_ld_path}",  # added by wrapper
                    f"--sysroot=../some/random/sysroot",
                ]
            ),
        )
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs,
            set([remote_compiler, shlib_rel, libcxx, source] + deps),
        )
        self.assertEqual(remote_output_files, {rlib})

    def test_run_local_downloads_inputs_if_needed(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        input_rlib = Path("libintermediate.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_path = Path("obj/foo.rlib.d")
        depfile_contents = [str(d) + ":" for d in deps]
        command = _strs(
            [compiler, source, "-o", rlib, f"--emit=dep-info={depfile_path}"]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            [f"--inputs={input_rlib}", "--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        prepare_mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in prepare_mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source, input_rlib] + deps)
        )
        self.assertEqual(remote_output_files, {rlib, depfile_path})

        with mock.patch.object(
            remote_action.RemoteAction, "download_inputs", return_value={}
        ) as mock_download_inputs:
            with mock.patch.object(
                subprocess, "call", return_value=0
            ) as mock_call:
                run_status = r.run_local()

        mock_call.assert_called_once()
        self.assertEqual(run_status, 0)
        mock_download_inputs.assert_called_once()

    def test_run_remote_downloads_link_repro_on_failure(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        link_repro_path = Path("debug/lld-crash.tar")
        depfile_contents = [str(d) + ":" for d in deps]
        command = _strs(
            [
                compiler,
                source,
                "-o",
                rlib,
                f"-Clink-arg=--reproduce={link_repro_path}",
            ]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )
        self.assertEqual(r._rust_action.link_reproducer, link_repro_path)

        prepare_mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in prepare_mocks:
                stack.enter_context(m)
            with mock.patch.object(
                remote_action.RemoteAction, "run_with_main_args", return_value=1
            ) as mock_run:
                with mock.patch.object(
                    remote_action.RemoteAction,
                    "download_output_file",
                    return_value=0,
                ) as mock_download:
                    run_status = r.run()

        self.assertEqual(run_status, 1)  # failure
        mock_run.assert_called_once()
        mock_download.assert_called_once_with(link_repro_path)

    def test_post_run_actions(self) -> None:
        exec_root = Path("/home/project")
        working_dir = exec_root / "build-here"
        compiler = Path("../tools/bin/rustc")
        shlib = Path("tools/lib/librusteze.so")
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        deps = [Path("../foo/src/other.rs")]
        depfile_path = Path("obj/foo.rlib.d")
        depfile_contents = [str(d) + ":" for d in deps]
        command = _strs(
            [compiler, source, "-o", rlib, f"--emit=dep-info={depfile_path}"]
        )
        r = rustc_remote_wrapper.RustRemoteAction(
            ["--", *command],
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        prepare_mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in prepare_mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        self.assertIsNotNone(a._post_remote_run_success_action)
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps)
        )
        self.assertEqual(remote_output_files, {rlib, depfile_path})

        run_mocks = [
            mock.patch.object(
                remote_action.RemoteAction,
                "_run_maybe_remotely",
                return_value=cl_utils.SubprocessResult(0),
            ),
            mock.patch.object(
                rustc_remote_wrapper.RustRemoteAction,
                "_depfile_exists",
                return_value=True,
            ),
            mock.patch.object(
                rustc_remote_wrapper.RustRemoteAction, "_cleanup"
            ),
        ]
        with contextlib.ExitStack() as stack:
            for m in run_mocks:
                stack.enter_context(m)
            with mock.patch.object(
                rustc_remote_wrapper.RustRemoteAction,
                "_rewrite_remote_or_local_depfile",
            ) as mock_rewrite:
                r.run()
            mock_rewrite.assert_called_with()

    def test_rewrite_remote_depfile(self) -> None:
        compiler = Path("../tools/bin/rustc")
        source = Path("../foo/src/lib.rs")
        rlib = Path("obj/foo.rlib")
        depfile_path = Path("obj/foo.rlib.d")
        with tempfile.TemporaryDirectory() as td:
            exec_root = Path(td)
            working_dir = exec_root / "build-here"
            command = _strs(
                [
                    compiler,
                    source,
                    "-o",
                    rlib,
                    f"--emit=dep-info={depfile_path}",
                ]
            )
            r = rustc_remote_wrapper.RustRemoteAction(
                ["--canonicalize_working_dir=true", "--", *command],
                exec_root=exec_root,
                working_dir=working_dir,
                auto_reproxy=False,
            )

            prepare_mocks = self.generate_prepare_mocks()
            with contextlib.ExitStack() as stack:
                for m in prepare_mocks:
                    stack.enter_context(m)

                with mock.patch.object(
                    rustc_remote_wrapper.RustRemoteAction,
                    "_remote_inputs",
                    return_value=iter([]),
                ) as mock_inputs:
                    prepare_status = r.prepare()

            mock_inputs.assert_called_with()
            self.assertEqual(prepare_status, 0)  # success
            remote_cwd = r.remote_action.remote_working_dir
            depfile_abspath = working_dir / depfile_path
            depfile_abspath.parent.mkdir(parents=True, exist_ok=True)
            _write_file_contents(
                depfile_abspath,
                f"{remote_cwd}/obj/foo.rlib: {remote_cwd}/src/lib.rs\n",
            )
            r._rewrite_remote_or_local_depfile()
            new_deps = _read_file_contents(depfile_abspath)
            self.assertEqual(new_deps, "obj/foo.rlib: src/lib.rs\n")


class MainTests(unittest.TestCase):
    def test_help_implicit(self) -> None:
        # Just make sure help exits successfully, without any exceptions
        # due to argument parsing.
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(
                sys, "exit", side_effect=ImmediateExit
            ) as mock_exit:
                with self.assertRaises(ImmediateExit):
                    rustc_remote_wrapper.main([])
        mock_exit.assert_called_with(0)

    def test_help_flag(self) -> None:
        # Just make sure help exits successfully, without any exceptions
        # due to argument parsing.
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(
                sys, "exit", side_effect=ImmediateExit
            ) as mock_exit:
                with self.assertRaises(ImmediateExit):
                    rustc_remote_wrapper.main(["--help"])
        mock_exit.assert_called_with(0)

    def test_auto_relaunched_with_reproxy(self) -> None:
        argv = ["--", "rustc", "foo.rs", "-o", "foo.rlib"]
        with mock.patch.object(
            os.environ, "get", return_value=None
        ) as mock_env:
            with mock.patch.object(
                cl_utils, "exec_relaunch", side_effect=ImmediateExit
            ) as mock_relaunch:
                with self.assertRaises(ImmediateExit):
                    rustc_remote_wrapper.main(argv)
        mock_env.assert_called()
        mock_relaunch.assert_called_once()
        args, kwargs = mock_relaunch.call_args_list[0]
        relaunch_cmd = args[0]
        self.assertEqual(relaunch_cmd[0], str(fuchsia.REPROXY_WRAP))
        cmd_slices = cl_utils.split_into_subsequences(relaunch_cmd[1:], "--")
        reproxy_args, self_script, wrapped_command = cmd_slices
        self.assertEqual(reproxy_args, ["-v"])
        self.assertIn("python", self_script[0])
        self.assertTrue(self_script[-1].endswith("rustc_remote_wrapper.py"))
        self.assertEqual(wrapped_command, argv[1:])


if __name__ == "__main__":
    remote_action.init_from_main_once()
    unittest.main()
