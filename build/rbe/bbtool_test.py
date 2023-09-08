#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import contextlib
import io
import unittest
from pathlib import Path
from unittest import mock

import bbtool
import cl_utils


class MainArgParserTests(unittest.TestCase):

    def test_fetch_reproxy_log(self):
        args = bbtool._MAIN_ARG_PARSER.parse_args(
            ['--bbid', '1234', 'fetch_reproxy_log'])
        self.assertEqual(args.bbid, '1234')
        self.assertFalse(args.verbose)


class BuildBucketToolTests(unittest.TestCase):

    def test_get_json_fields_success(self):
        bb = bbtool.BuildBucketTool()
        with mock.patch.object(cl_utils, 'subprocess_call',
                               return_value=cl_utils.SubprocessResult(
                                   0, stdout=['{}'])) as mock_call:
            bb_json = bb.get_json_fields('5432')
            self.assertEqual(bb_json, dict())
        mock_call.assert_called_once()

    def test_get_json_fields_error(self):
        bb = bbtool.BuildBucketTool()
        with mock.patch.object(cl_utils, 'subprocess_call',
                               return_value=cl_utils.SubprocessResult(
                                   1, stderr=['oops'])) as mock_call:
            with self.assertRaises(bbtool.BBError):
                bb.get_json_fields('6432')
        mock_call.assert_called_once()

    def test_download_reproxy_log_success(self):
        bb = bbtool.BuildBucketTool()
        with mock.patch.object(
                cl_utils, 'subprocess_call',
                return_value=cl_utils.SubprocessResult(0)) as mock_call:
            self.assertEqual(
                bb.download_reproxy_log('1271', 'reproxy_3.1415926.rrpl'), '\n')
        mock_call.assert_called_once()

    def test_download_reproxy_log_error(self):
        bb = bbtool.BuildBucketTool()
        with mock.patch.object(
                cl_utils, 'subprocess_call',
                return_value=cl_utils.SubprocessResult(1)) as mock_call:
            with self.assertRaises(bbtool.BBError):
                bb.download_reproxy_log('1271', 'reproxy_3.1415926.rrpl')
        mock_call.assert_called_once()

    def test_fetch_reproxy_log_cached_already_cached(self):
        log_name = 'reproxy_1.4142.rrpl'
        bb = bbtool.BuildBucketTool()
        with mock.patch.object(Path, 'mkdir') as mock_mkdir:
            with mock.patch.object(Path, 'exists',
                                   return_value=True) as mock_exists:
                cached_path = bb.fetch_reproxy_log_cached('132461', log_name)
                self.assertEqual(cached_path.name, log_name)  # in some temp dir
        mock_mkdir.assert_called_once()
        mock_exists.assert_called_with()

    def test_fetch_reproxy_log_cached_download_success(self):
        log_name = 'reproxy_9.6142.rrpl'
        log_contents = '\n'
        bb = bbtool.BuildBucketTool()
        with mock.patch.object(Path, 'mkdir') as mock_mkdir:
            with mock.patch.object(Path, 'exists',
                                   return_value=False) as mock_exists:
                with mock.patch.object(
                        bbtool.BuildBucketTool, 'download_reproxy_log',
                        return_value=log_contents) as mock_download:
                    with mock.patch.object(Path, 'write_text') as mock_write:
                        cached_path = bb.fetch_reproxy_log_cached(
                            '662481', log_name)
                        self.assertEqual(
                            cached_path.name, log_name)  # in some temp dir
        mock_mkdir.assert_called_once()
        mock_exists.assert_called_with()
        mock_write.assert_called_with(log_contents)

    def test_fetch_reproxy_log_cached_download_error(self):
        log_name = 'reproxy_7.0123.rrpl'
        bb = bbtool.BuildBucketTool()
        with mock.patch.object(Path, 'mkdir') as mock_mkdir:
            with mock.patch.object(Path, 'exists',
                                   return_value=False) as mock_exists:
                with mock.patch.object(bbtool.BuildBucketTool,
                                       'download_reproxy_log',
                                       side_effect=bbtool.BBError(
                                           'download error')) as mock_download:
                    with self.assertRaises(bbtool.BBError):
                        bb.fetch_reproxy_log_cached('662481', log_name)
        mock_mkdir.assert_called_once()
        mock_exists.assert_called_with()

    def test_get_rbe_build_info_no_child(self):
        bb = bbtool.BuildBucketTool()
        bbid = '8888'
        build_json = {'output': {'properties': {}}}
        with mock.patch.object(bbtool.BuildBucketTool, 'get_json_fields',
                               return_value=build_json) as mock_get_json:
            rbe_build_id, rbe_build_json = bb.get_rbe_build_info(bbid)
            self.assertEqual(rbe_build_id, bbid)
            self.assertEqual(rbe_build_json, build_json)

    def test_get_rbe_build_info_with_child(self):
        bb = bbtool.BuildBucketTool()
        parent_bbid = '9977'
        child_bbid = '0404'
        parent_build_json = {
            'output': {
                'properties': {
                    'child_build_id': child_bbid
                }
            }
        }
        child_build_json = {
            'output': {
                'properties': {
                    'rpl_files': ['foo.rrpl']
                }
            }
        }
        with mock.patch.object(bbtool.BuildBucketTool, 'get_json_fields',
                               side_effect=[parent_build_json,
                                            child_build_json]) as mock_get_json:
            rbe_build_id, rbe_build_json = bb.get_rbe_build_info(parent_bbid)
            self.assertEqual(rbe_build_id, child_bbid)
            self.assertEqual(rbe_build_json, child_build_json)


class FetchReproxyLogFromBbidTests(unittest.TestCase):

    def test_lookup_and_fetch(self):
        bb_json = {
            'output': {
                'properties': {
                    'rpl_files': ['reproxy_2.718.rrpl']
                }
            }
        }
        reproxy_log_path = Path('/some/where/in/temp/foo.rrpl')
        with mock.patch.object(bbtool.BuildBucketTool, 'get_rbe_build_info',
                               return_value=('8728721',
                                             bb_json)) as mock_rbe_build_info:
            with mock.patch.object(
                    bbtool.BuildBucketTool, 'fetch_reproxy_log_cached',
                    return_value=reproxy_log_path) as mock_fetch_log:
                self.assertEqual(
                    bbtool.fetch_reproxy_log_from_bbid(
                        bbpath=Path('bb'), bbid='b789789'), reproxy_log_path)
        mock_rbe_build_info.assert_called_once()
        mock_fetch_log.assert_called_once()

    def test_no_rpl_files(self):
        bb_json = {'output': {'properties': {}}}
        with mock.patch.object(bbtool.BuildBucketTool, 'get_rbe_build_info',
                               return_value=('8728721',
                                             bb_json)) as mock_rbe_build_info:
            self.assertIsNone(
                bbtool.fetch_reproxy_log_from_bbid(
                    bbpath=Path('bb'), bbid='b789789'))
        mock_rbe_build_info.assert_called_once()


class MainTests(unittest.TestCase):

    def test_e2e_bbid_to_log_only(self):
        bb = bbtool._BB_TOOL
        bbid = 'b991918261'
        reproxy_log_path = Path('/path/to/cache/foobar.rrpl')
        result = io.StringIO()
        with contextlib.redirect_stdout(result):
            with mock.patch.object(
                    bbtool, 'fetch_reproxy_log_from_bbid',
                    return_value=reproxy_log_path) as mock_fetch_log:
                self.assertEqual(
                    bbtool.main(['--bbid', bbid, 'fetch_reproxy_log']), 0)
        self.assertTrue(
            result.getvalue().endswith(
                f"reproxy log name: {reproxy_log_path}\n"))
        mock_fetch_log.assert_called_once_with(
            bbpath=bb, bbid=bbid.lstrip('b'), verbose=False)

    def test_e2e_bb_error(self):
        bbid = 'b99669966'
        with mock.patch.object(
                bbtool.BuildBucketTool, 'get_rbe_build_info',
                side_effect=bbtool.BBError('error')) as mock_rbe_build_info:
            self.assertEqual(
                bbtool.main(['--bbid', bbid, 'fetch_reproxy_log']), 1)
        mock_rbe_build_info.assert_called_once_with(
            bbid.lstrip('b'), verbose=False)


if __name__ == '__main__':
    unittest.main()
