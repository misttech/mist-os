#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import dlwrap

import cl_utils
import remote_action
import remotetool


class MainArgParserTests(unittest.TestCase):

    def test_defaults(self):
        args = dlwrap._MAIN_ARG_PARSER.parse_args([])
        self.assertFalse(args.verbose)
        self.assertFalse(args.dry_run)
        self.assertEqual(args.download, [])
        self.assertEqual(args.command, [])

    def test_verbose(self):
        args = dlwrap._MAIN_ARG_PARSER.parse_args(['--verbose'])
        self.assertTrue(args.verbose)

    def test_dry_run(self):
        args = dlwrap._MAIN_ARG_PARSER.parse_args(['--dry-run'])
        self.assertTrue(args.dry_run)

    def test_download_list(self):
        args = dlwrap._MAIN_ARG_PARSER.parse_args(
            ['--download', 'aa.o', 'bb.o'])
        self.assertEqual(args.download, [Path('aa.o'), Path('bb.o')])

    def test_command(self):
        args = dlwrap._MAIN_ARG_PARSER.parse_args(['--', 'cat', 'dog.txt'])
        self.assertEqual(args.command, ['cat', 'dog.txt'])


class ReadDownloadStubInfosTests(unittest.TestCase):

    def test_is_a_stub(self):
        path = 'path/to/no/where.obj'
        stub_info = remote_action.DownloadStubInfo(
            path=path,
            type="file",
            blob_digest='feeeeeeedfaaaaace/124',
            action_digest='87ac8eb9865d/43',
            build_id='10293-ab8e-bc72',
        )
        with mock.patch.object(remote_action, 'is_download_stub_file',
                               return_value=True) as mock_is_stub:
            with mock.patch.object(remote_action.DownloadStubInfo,
                                   'read_from_file',
                                   return_value=stub_info) as mock_read:
                stub_infos = list(dlwrap.read_download_stub_infos([path]))
        self.assertEqual(stub_infos, [stub_info])
        mock_is_stub.assert_called_with(path)
        mock_read.assert_called_with(path)

    def test_not_a_stub(self):
        path = 'path/to/already/downloaded.obj'
        with mock.patch.object(remote_action, 'is_download_stub_file',
                               return_value=False) as mock_is_stub:
            stub_infos = list(dlwrap.read_download_stub_infos([path]))
        self.assertEqual(stub_infos, [])
        mock_is_stub.assert_called_with(path)


_fake_downloader = remotetool.RemoteTool(
    reproxy_cfg={
        "service": "foo.build.service:443",
        "instance": "my-project/remote/instances/default",
    })


class DownloadArtifactsTests(unittest.TestCase):

    def _stub_info(self, path: Path) -> remote_action.DownloadStubInfo:
        return remote_action.DownloadStubInfo(
            path=path,
            type="file",
            blob_digest='f33333df44444ce/124',
            action_digest='47ac8eb38351/41',
            build_id='50f93-a38e-b112',
        )

    def test_success(self):
        path = 'road/to/perdition.obj'
        exec_root = Path('/exec/root')
        working_dir = exec_root / 'work'
        download_status = 0
        with mock.patch.object(dlwrap, 'read_download_stub_infos',
                               return_value=iter([self._stub_info(path)
                                                 ])) as mock_read_stubs:
            with mock.patch.object(remote_action.DownloadStubInfo, 'download',
                                   return_value=cl_utils.SubprocessResult(
                                       download_status)) as mock_download:
                status = dlwrap.download_artifacts(
                    [path],
                    downloader=_fake_downloader,
                    working_dir_abs=working_dir)
        self.assertEqual(status, download_status)
        mock_read_stubs.assert_called_once()
        mock_download.assert_called_with(
            downloader=_fake_downloader, working_dir_abs=working_dir)

    def test_failure(self):
        path = 'highway/to/hell.obj'
        exec_root = Path('/exec/root')
        working_dir = exec_root / 'work'
        download_status = 1
        with mock.patch.object(dlwrap, 'read_download_stub_infos',
                               return_value=iter([self._stub_info(path)
                                                 ])) as mock_read_stubs:
            with mock.patch.object(remote_action.DownloadStubInfo, 'download',
                                   return_value=cl_utils.SubprocessResult(
                                       download_status)) as mock_download:
                status = dlwrap.download_artifacts(
                    [path],
                    downloader=_fake_downloader,
                    working_dir_abs=working_dir)
        self.assertEqual(status, download_status)
        mock_read_stubs.assert_called_once()
        mock_download.assert_called_with(
            downloader=_fake_downloader, working_dir_abs=working_dir)


class MainTests(unittest.TestCase):

    def test_dry_run(self):
        path = 'dir/file.o'
        exec_root = Path('/exec/root')
        working_dir = exec_root / 'work'
        with mock.patch.object(dlwrap, 'download_artifacts',
                               return_value=0) as mock_download:
            with mock.patch.object(subprocess, 'call') as mock_run:
                status = dlwrap._main(
                    ['--dry-run', '--', 'cat', 'foo'],
                    downloader=_fake_downloader,
                    working_dir_abs=working_dir)
        self.assertEqual(status, 0)
        mock_run.assert_not_called()

    def test_download_fail(self):
        path = 'dir/file.o'
        exec_root = Path('/exec/root')
        working_dir = exec_root / 'work'
        with mock.patch.object(dlwrap, 'download_artifacts',
                               return_value=1) as mock_download:
            with mock.patch.object(subprocess, 'call') as mock_run:
                status = dlwrap._main(
                    ['--', 'cat', 'foo'],
                    downloader=_fake_downloader,
                    working_dir_abs=working_dir)
        self.assertEqual(status, 1)
        mock_run.assert_not_called()

    def test_no_command(self):
        path = 'dir/file.o'
        exec_root = Path('/exec/root')
        working_dir = exec_root / 'work'
        with mock.patch.object(dlwrap, 'download_artifacts',
                               return_value=0) as mock_download:
            with mock.patch.object(subprocess, 'call',
                                   return_value=0) as mock_run:
                status = dlwrap._main(
                    [],
                    downloader=_fake_downloader,
                    working_dir_abs=working_dir)
        self.assertEqual(status, 0)
        mock_run.assert_not_called()

    def test_success(self):
        path = 'dir/file.o'
        exec_root = Path('/exec/root')
        working_dir = exec_root / 'work'
        with mock.patch.object(dlwrap, 'download_artifacts',
                               return_value=0) as mock_download:
            with mock.patch.object(subprocess, 'call',
                                   return_value=0) as mock_run:
                status = dlwrap._main(
                    ['--', 'cat', 'foo'],
                    downloader=_fake_downloader,
                    working_dir_abs=working_dir)
        self.assertEqual(status, 0)
        mock_run.assert_called_with(['cat', 'foo'])


if __name__ == '__main__':
    unittest.main()
