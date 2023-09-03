#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import contextlib
import io
import unittest
from pathlib import Path
from unittest import mock

import bb_fetch_rbe_cas
import cl_utils
import remotetool
import reproxy_logs


class MainArgParserTests(unittest.TestCase):

    def test_with_bbid(self):
        args = bb_fetch_rbe_cas._MAIN_ARG_PARSER.parse_args(
            ['--bbid', '1234', '--path', 'obj/hello.o'])
        self.assertEqual(args.bbid, '1234')
        self.assertEqual(args.path, Path('obj/hello.o'))
        self.assertIsNone(args.output)
        self.assertFalse(args.verbose)
        self.assertIsNone(args.reproxy_log)

    def test_with_reproxy_log(self):
        args = bb_fetch_rbe_cas._MAIN_ARG_PARSER.parse_args(
            ['--reproxy_log', 'x.rrpl', '--path', 'obj/hello.o', '--verbose'])
        self.assertIsNone(args.bbid)
        self.assertEqual(args.path, Path('obj/hello.o'))
        self.assertIsNone(args.output)
        self.assertTrue(args.verbose)
        self.assertEqual(args.reproxy_log, Path('x.rrpl'))


class BuildBucketToolTests(unittest.TestCase):

    def test_get_json_fields_success(self):
        bb = bb_fetch_rbe_cas.BuildBucketTool()
        with mock.patch.object(cl_utils, 'subprocess_call',
                               return_value=cl_utils.SubprocessResult(
                                   0, stdout=['{}'])) as mock_call:
            bb_json = bb.get_json_fields('5432')
            self.assertEqual(bb_json, dict())
        mock_call.assert_called_once()

    def test_get_json_fields_error(self):
        bb = bb_fetch_rbe_cas.BuildBucketTool()
        with mock.patch.object(cl_utils, 'subprocess_call',
                               return_value=cl_utils.SubprocessResult(
                                   1, stderr=['oops'])) as mock_call:
            with self.assertRaises(bb_fetch_rbe_cas.BBError):
                bb.get_json_fields('6432')
        mock_call.assert_called_once()

    def test_download_reproxy_log_success(self):
        bb = bb_fetch_rbe_cas.BuildBucketTool()
        with mock.patch.object(
                cl_utils, 'subprocess_call',
                return_value=cl_utils.SubprocessResult(0)) as mock_call:
            self.assertEqual(
                bb.download_reproxy_log('1271', 'reproxy_3.1415926.rrpl'), '\n')
        mock_call.assert_called_once()

    def test_download_reproxy_log_error(self):
        bb = bb_fetch_rbe_cas.BuildBucketTool()
        with mock.patch.object(
                cl_utils, 'subprocess_call',
                return_value=cl_utils.SubprocessResult(1)) as mock_call:
            with self.assertRaises(bb_fetch_rbe_cas.BBError):
                bb.download_reproxy_log('1271', 'reproxy_3.1415926.rrpl')
        mock_call.assert_called_once()

    def test_fetch_reproxy_log_cached_already_cached(self):
        log_name = 'reproxy_1.4142.rrpl'
        bb = bb_fetch_rbe_cas.BuildBucketTool()
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
        bb = bb_fetch_rbe_cas.BuildBucketTool()
        with mock.patch.object(Path, 'mkdir') as mock_mkdir:
            with mock.patch.object(Path, 'exists',
                                   return_value=False) as mock_exists:
                with mock.patch.object(
                        bb_fetch_rbe_cas.BuildBucketTool,
                        'download_reproxy_log',
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
        bb = bb_fetch_rbe_cas.BuildBucketTool()
        with mock.patch.object(Path, 'mkdir') as mock_mkdir:
            with mock.patch.object(Path, 'exists',
                                   return_value=False) as mock_exists:
                with mock.patch.object(bb_fetch_rbe_cas.BuildBucketTool,
                                       'download_reproxy_log',
                                       side_effect=bb_fetch_rbe_cas.BBError(
                                           'download error')) as mock_download:
                    with self.assertRaises(bb_fetch_rbe_cas.BBError):
                        bb.fetch_reproxy_log_cached('662481', log_name)
        mock_mkdir.assert_called_once()
        mock_exists.assert_called_with()

    def test_get_rbe_build_info_no_child(self):
        bb = bb_fetch_rbe_cas.BuildBucketTool()
        bbid = '8888'
        build_json = {'output': {'properties': {}}}
        with mock.patch.object(bb_fetch_rbe_cas.BuildBucketTool,
                               'get_json_fields',
                               return_value=build_json) as mock_get_json:
            rbe_build_id, rbe_build_json = bb.get_rbe_build_info(bbid)
            self.assertEqual(rbe_build_id, bbid)
            self.assertEqual(rbe_build_json, build_json)

    def test_get_rbe_build_info_with_child(self):
        bb = bb_fetch_rbe_cas.BuildBucketTool()
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
        with mock.patch.object(bb_fetch_rbe_cas.BuildBucketTool,
                               'get_json_fields',
                               side_effect=[parent_build_json,
                                            child_build_json]) as mock_get_json:
            rbe_build_id, rbe_build_json = bb.get_rbe_build_info(parent_bbid)
            self.assertEqual(rbe_build_id, child_bbid)
            self.assertEqual(rbe_build_json, child_build_json)


_reproxy_cfg = {
    'service': 'some.remote.service.com:443',
    'instance': 'projects/my-project/instance/default',
}


class DownloadArtifactTests(unittest.TestCase):

    @property
    def downloader(self):
        return remotetool.RemoteTool(reproxy_cfg=_reproxy_cfg)

    def test_success(self):
        with mock.patch.object(
                remotetool.RemoteTool, 'download_blob',
                return_value=cl_utils.SubprocessResult(0)) as mock_download:
            exit_code = bb_fetch_rbe_cas.download_artifact(
                self.downloader, '8787def87e90/434', Path('storage/bin'))
            self.assertEqual(exit_code, 0)

    def test_failure(self):
        with mock.patch.object(
                remotetool.RemoteTool, 'download_blob',
                return_value=cl_utils.SubprocessResult(1)) as mock_download:
            exit_code = bb_fetch_rbe_cas.download_artifact(
                self.downloader, '1c1df1000/93', Path('shipping/dock'))
            self.assertEqual(exit_code, 1)


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
        with mock.patch.object(bb_fetch_rbe_cas.BuildBucketTool,
                               'get_rbe_build_info',
                               return_value=('8728721',
                                             bb_json)) as mock_rbe_build_info:
            with mock.patch.object(
                    bb_fetch_rbe_cas.BuildBucketTool,
                    'fetch_reproxy_log_cached',
                    return_value=reproxy_log_path) as mock_fetch_log:
                self.assertEqual(
                    bb_fetch_rbe_cas.fetch_reproxy_log_from_bbid(
                        bbpath=Path('bb'), bbid='b789789'), reproxy_log_path)
        mock_rbe_build_info.assert_called_once()
        mock_fetch_log.assert_called_once()

    def test_no_rpl_files(self):
        bb_json = {'output': {'properties': {}}}
        with mock.patch.object(bb_fetch_rbe_cas.BuildBucketTool,
                               'get_rbe_build_info',
                               return_value=('8728721',
                                             bb_json)) as mock_rbe_build_info:
            self.assertIsNone(
                bb_fetch_rbe_cas.fetch_reproxy_log_from_bbid(
                    bbpath=Path('bb'), bbid='b789789'))
        mock_rbe_build_info.assert_called_once()


class FetchArtifactFromReproxyLogTests(unittest.TestCase):

    @property
    def downloader(self):
        return remotetool.RemoteTool(reproxy_cfg=_reproxy_cfg)

    def test_digest_found(self):
        reproxy_log = Path('cached.rrpl')
        artifact_path = Path('obj/hello.cc.o')
        cfg = Path('reproxy.cfg')
        digest = 'f1233c9744d/101'
        output = Path('fetched-hello.cc.o')
        status = 0
        with mock.patch.object(reproxy_logs, 'lookup_output_file_digest',
                               return_value=digest) as mock_lookup:
            with mock.patch.object(
                    bb_fetch_rbe_cas, 'rbe_downloader',
                    return_value=self.downloader) as mock_downloader:
                with mock.patch.object(bb_fetch_rbe_cas, 'download_artifact',
                                       return_value=status) as mock_download:
                    self.assertEqual(
                        bb_fetch_rbe_cas.fetch_artifact_from_reproxy_log(
                            reproxy_log=reproxy_log,
                            artifact_path=artifact_path,
                            cfg=cfg,
                            output=output,
                        ), status)
        mock_lookup.assert_called_once_with(log=reproxy_log, path=artifact_path)
        mock_downloader.assert_called_once_with(cfg)
        mock_download.assert_called_once_with(self.downloader, digest, output)

    def test_digest_not_found(self):
        reproxy_log = Path('cached.rrpl')
        artifact_path = Path('obj/hello.cc.o')
        with mock.patch.object(reproxy_logs, 'lookup_output_file_digest',
                               return_value=None) as mock_lookup:
            self.assertEqual(
                bb_fetch_rbe_cas.fetch_artifact_from_reproxy_log(
                    reproxy_log=reproxy_log,
                    artifact_path=artifact_path,
                    cfg=Path('reproxy.cfg'),
                    output=Path('fetched-hello.cc.o'),
                ), 1)
        mock_lookup.assert_called_once_with(log=reproxy_log, path=artifact_path)

    def test_download_failed(self):
        reproxy_log = Path('smashed.rrpl')
        artifact_path = Path('obj/h3llo.cc.o')
        cfg = Path('reproxy_2.cfg')
        output = Path('fetched-h3llo.cc.o')
        digest = 'a87e1321/88'
        status = 1
        with mock.patch.object(reproxy_logs, 'lookup_output_file_digest',
                               return_value=digest) as mock_lookup:
            with mock.patch.object(
                    bb_fetch_rbe_cas, 'rbe_downloader',
                    return_value=self.downloader) as mock_downloader:
                with mock.patch.object(bb_fetch_rbe_cas, 'download_artifact',
                                       return_value=status) as mock_download:
                    self.assertEqual(
                        bb_fetch_rbe_cas.fetch_artifact_from_reproxy_log(
                            reproxy_log=reproxy_log,
                            artifact_path=artifact_path,
                            cfg=cfg,
                            output=output,
                        ), status)
        mock_lookup.assert_called_once_with(log=reproxy_log, path=artifact_path)
        mock_downloader.assert_called_once_with(cfg)
        mock_download.assert_called_once_with(self.downloader, digest, output)


class MainTests(unittest.TestCase):

    def test_e2e_using_bbid_success(self):
        bb = bb_fetch_rbe_cas._BB_TOOL
        bbid = 'b4356254'
        artifact_path = Path('foo/bar/baz.rlib')
        cfg = bb_fetch_rbe_cas._REPROXY_CFG
        reproxy_log_path = Path('/some/where/in/temp/foo.rrpl')
        with mock.patch.object(bb_fetch_rbe_cas, 'fetch_reproxy_log_from_bbid',
                               return_value=reproxy_log_path) as mock_fetch_log:
            with mock.patch.object(bb_fetch_rbe_cas,
                                   'fetch_artifact_from_reproxy_log',
                                   return_value=0) as mock_fetch_artifact:
                self.assertEqual(
                    bb_fetch_rbe_cas.main(
                        ['--bbid', bbid, '--path',
                         str(artifact_path)]), 0)
        mock_fetch_log.assert_called_once_with(
            bbpath=bb, bbid=bbid.lstrip('b'), verbose=False)
        mock_fetch_artifact.assert_called_once_with(
            reproxy_log=reproxy_log_path,
            artifact_path=artifact_path,
            cfg=cfg,
            output=artifact_path.name,
            verbose=False)

    def test_e2e_bbid_to_log_only(self):
        bb = bb_fetch_rbe_cas._BB_TOOL
        bbid = 'b991918261'
        reproxy_log_path = Path('/path/to/cache/foobar.rrpl')
        result = io.StringIO()
        with contextlib.redirect_stdout(result):
            with mock.patch.object(
                    bb_fetch_rbe_cas, 'fetch_reproxy_log_from_bbid',
                    return_value=reproxy_log_path) as mock_fetch_log:
                self.assertEqual(bb_fetch_rbe_cas.main(['--bbid', bbid]), 0)
        self.assertTrue(
            result.getvalue().endswith(f"reproxy log: {reproxy_log_path}\n"))
        mock_fetch_log.assert_called_once_with(
            bbpath=bb, bbid=bbid.lstrip('b'), verbose=False)

    def test_e2e_using_reproxy_log_success(self):
        bb = bb_fetch_rbe_cas._BB_TOOL
        artifact_path = Path('foo/bar/baz.rlib')
        cfg = bb_fetch_rbe_cas._REPROXY_CFG
        reproxy_log_path = Path('use/me.rrpl')
        with mock.patch.object(bb_fetch_rbe_cas,
                               'fetch_artifact_from_reproxy_log',
                               return_value=0) as mock_fetch_artifact:
            self.assertEqual(
                bb_fetch_rbe_cas.main(
                    [
                        '--reproxy_log',
                        str(reproxy_log_path), '--path',
                        str(artifact_path)
                    ]), 0)
        # bypassed call to fetch_reproxy_log_from_bbid
        mock_fetch_artifact.assert_called_once_with(
            reproxy_log=reproxy_log_path,
            artifact_path=artifact_path,
            cfg=cfg,
            output=artifact_path.name,
            verbose=False)

    def test_e2e_reproxy_log_failed(self):
        bb = bb_fetch_rbe_cas._BB_TOOL
        bbid = 'b4356254'
        with mock.patch.object(bb_fetch_rbe_cas, 'fetch_reproxy_log_from_bbid',
                               return_value=None) as mock_fetch_log:
            self.assertEqual(
                bb_fetch_rbe_cas.main(
                    ['--bbid', bbid, '--path', 'foo/bar/baz.rlib']), 1)
        mock_fetch_log.assert_called_once_with(
            bbpath=bb, bbid=bbid.lstrip('b'), verbose=False)

    def test_e2e_bb_error(self):
        bbid = 'b99669966'
        with mock.patch.object(bb_fetch_rbe_cas.BuildBucketTool,
                               'get_rbe_build_info',
                               side_effect=bb_fetch_rbe_cas.BBError(
                                   'error')) as mock_rbe_build_info:
            self.assertEqual(
                bb_fetch_rbe_cas.main(
                    ['--bbid', bbid, '--path', 'zoo/bar/faz.rlib']), 1)
        mock_rbe_build_info.assert_called_once_with(
            bbid.lstrip('b'), verbose=False)

    def test_e2e_download_artifact_error(self):
        bb = bb_fetch_rbe_cas._BB_TOOL
        bbid = 'b881231281'
        reproxy_log_path = Path('/some/where/in/temp/foo.rrpl')
        artifact_path = Path('foo/bar/baz.rlib')
        cfg = bb_fetch_rbe_cas._REPROXY_CFG
        status = 1
        with mock.patch.object(bb_fetch_rbe_cas, 'fetch_reproxy_log_from_bbid',
                               return_value=reproxy_log_path) as mock_fetch_log:
            with mock.patch.object(bb_fetch_rbe_cas,
                                   'fetch_artifact_from_reproxy_log',
                                   return_value=status) as mock_fetch_artifact:
                self.assertEqual(
                    bb_fetch_rbe_cas.main(
                        ['--bbid', bbid, '--path',
                         str(artifact_path)]), status)
        mock_fetch_log.assert_called_once_with(
            bbpath=bb, bbid=bbid.lstrip('b'), verbose=False)
        mock_fetch_artifact.assert_called_once_with(
            reproxy_log=reproxy_log_path,
            artifact_path=artifact_path,
            cfg=cfg,
            output=artifact_path.name,
            verbose=False)


if __name__ == '__main__':
    unittest.main()
