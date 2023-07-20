# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import subprocess
import sys
import tempfile
import textwrap
from threading import Event, Thread
import unittest

TEST_DATA_DIR = 'host_x64/test_data/fidlcat_e2e_tests'  # relative to $PWD
FIDLCAT_TIMEOUT = 60  # timeout when invoking fidlcat

# Convert FUCHSIA_SSH_KEY into an absolute path. Otherwise ffx cannot find
# key and complains "Timeout attempting to reach target".
# See fxbug.dev/101081.
os.environ.update(
    FUCHSIA_ANALYTICS_DISABLED='1',
    FUCHSIA_SSH_KEY=os.path.abspath(os.environ['FUCHSIA_SSH_KEY']),
)


class Ffx:
    _path = 'host_x64/ffx'
    _args = ['--target', os.environ['FUCHSIA_DEVICE_ADDR']]

    def __init__(self, *args: str):
        self.process = subprocess.Popen(
            [self._path] + self._args + list(args),
            text=True,
            stdout=subprocess.PIPE)

    def wait(self):
        self.process.communicate()
        return self.process.returncode


class Fidlcat:
    _path = 'host_x64/fidlcat'
    _args = []
    _ffx_bridge = None

    def __init__(self, *args, merge_stderr=False):
        """
        merge_stderr: whether to merge stderr to stdout.
        """
        assert self._ffx_bridge is not None, 'must call setup first'
        stderr = subprocess.PIPE
        if merge_stderr:
            stderr = subprocess.STDOUT
        self.process = subprocess.Popen(
            [self._path] + self._args + list(args),
            text=True,
            stdout=subprocess.PIPE,
            stderr=stderr)
        self.stdout = ''  # Contains both stdout and stderr, if merge_stderr.
        self.stderr = ''
        self._timeout_cancel = Event()
        Thread(target=self._timeout_thread).start()

    def _timeout_thread(self):
        self._timeout_cancel.wait(FIDLCAT_TIMEOUT)
        if not self._timeout_cancel.is_set():
            self.process.kill()
            self.wait()
            raise TimeoutError('Fidlcat timeouts\n' + self.get_diagnose_msg())

    def wait(self):
        """Wait for the process to terminate, assert the returncode, fill the stdout and stderr."""
        (stdout, stderr) = self.process.communicate()
        self.stdout += stdout
        if stderr:  # None if merge_stderr
            self.stderr += stderr
        self._timeout_cancel.set()
        return self.process.returncode

    def read_until(self, pattern: str):
        """
        Read the stdout until EOF or a line contains pattern. Returns whether the pattern matches.

        Note: A deadlock could happen if we only read from stdout but the stderr buffer is full.
              Consider setting merge_stderr if you want to use this function.
        """
        while True:
            line = self.process.stdout.readline()
            if not line:
                return False
            self.stdout += line
            if pattern in line:
                return True

    def get_diagnose_msg(self):
        return '\n=== stdout ===\n' + self.stdout + '\n\n=== stderr===\n' + self.stderr + '\n'

    @classmethod
    def setup(cls):
        cls._ffx_bridge = Ffx('debug', 'connect', '--agent-only')
        socket_path = cls._ffx_bridge.process.stdout.readline().strip()
        assert os.path.exists(socket_path)
        cls._args = [
            '--unix-connect', socket_path, '--fidl-ir-path', TEST_DATA_DIR,
            '--symbol-path', TEST_DATA_DIR
        ]

    @classmethod
    def teardown(cls):
        cls._ffx_bridge.process.terminate()


# fuchsia-pkg URL for an echo realm. The echo realm contains echo client and echo server components.
# The echo client is an eager child of the realm and will start when the realm is started/run.
#
# Note that the actual echo client is in a standalone component echo_client.cm so we almost always
# need to specify "--remote-component=echo_client.cm" in the test cases below.
ECHO_REALM_URL = 'fuchsia-pkg://fuchsia.com/echo_realm_placeholder#meta/echo_realm.cm'
ECHO_REALM_MONIKER = '/core/ffx-laboratory:fidlcat_test_echo_realm'


class FidlcatE2eTests(unittest.TestCase):

    @classmethod
    def setUpClass(cls):
        Fidlcat.setup()

    @classmethod
    def tearDownClass(cls):
        Fidlcat.teardown()

    # Ensure debug_agent exits correctly after each test case. See fxbug.dev/101078.
    def tearDown(self):
        # FUCHSIA_DEVICE_ADDR and FUCHSIA_SSH_KEY must be defined.
        # FUCHSIA_SSH_PORT is only defined when invoked from `fx test`.
        cmd = [
            'ssh', '-F', 'none', '-o', 'CheckHostIP=no', '-o',
            'StrictHostKeyChecking=no', '-o', 'UserKnownHostsFile=/dev/null',
            '-i', os.environ['FUCHSIA_SSH_KEY']
        ]
        if os.environ.get('FUCHSIA_SSH_PORT'):
            cmd += ['-p', os.environ['FUCHSIA_SSH_PORT']]
        cmd += [
            os.environ['FUCHSIA_DEVICE_ADDR'], 'killall /pkg/bin/debug_agent'
        ]
        res = subprocess.run(
            cmd, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if res.returncode == 0 and 'Killed' in res.stdout:
            print('Killed dangling debug_agent', file=sys.stderr)
        else:
            # The return code will be 255 if no task found so don't check it.
            assert 'no tasks found' in res.stdout, res.stdout

    def test_run_echo(self):
        fidlcat = Fidlcat(
            '--remote-component=echo_client.cm', 'run', ECHO_REALM_URL)
        self.assertEqual(fidlcat.wait(), 0, fidlcat.get_diagnose_msg())

        self.assertIn(
            'sent request test.placeholders/Echo.EchoString = {\n'
            '    value: string = "hello world"\n'
            '  }', fidlcat.stdout)

    # TODO(fxbug.dev/113425): This test flakes on core.x64-debug, where fidlcat fails to exit after
    # receiving the SIGTERM signal.
    def disabled_test_stay_alive(self):
        fidlcat = Fidlcat(
            '--remote-name=echo_client', '--stay-alive', merge_stderr=True)
        fidlcat.read_until('Connected!')

        self.assertEqual(
            Ffx('component', 'run', ECHO_REALM_MONIKER, ECHO_REALM_URL).wait(),
            0)
        self.assertEqual(
            Ffx('component', 'destroy', ECHO_REALM_MONIKER).wait(), 0)
        fidlcat.read_until('Waiting for more processes to monitor.')

        # Because, with the --stay-alive version, fidlcat never ends,
        # we need to kill it to end the test.
        fidlcat.process.terminate()
        self.assertEqual(fidlcat.wait(), 0, fidlcat.get_diagnose_msg())

    def test_extra_component(self):
        fidlcat = Fidlcat(
            '--remote-component=echo_client.cm',
            '--extra-component=echo_server.cm', 'run', ECHO_REALM_URL)
        self.assertEqual(fidlcat.wait(), 0, fidlcat.get_diagnose_msg())

        self.assertIn('Monitoring echo_server.cm koid=', fidlcat.stdout)

    def test_trigger(self):
        fidlcat = Fidlcat(
            '--remote-component=echo_client.cm', '--trigger=.*EchoString',
            'run', ECHO_REALM_URL)
        self.assertEqual(fidlcat.wait(), 0, fidlcat.get_diagnose_msg())

        # The first displayed message must be EchoString.
        lines = fidlcat.stdout.split('\n\n')
        self.assertIn(
            'sent request test.placeholders/Echo.EchoString = {', lines[2])

    def test_messages(self):
        fidlcat = Fidlcat(
            '--remote-component=echo_client.cm', '--messages=.*EchoString',
            'run', ECHO_REALM_URL)
        self.assertEqual(fidlcat.wait(), 0, fidlcat.get_diagnose_msg())

        # The first and second displayed messages must be EchoString (everything else has been
        # filtered out).
        lines = fidlcat.stdout.split('\n\n')
        self.assertIn(
            'sent request test.placeholders/Echo.EchoString = {\n'
            '    value: string = "hello world"\n'
            '  }', lines[2])
        self.assertIn(
            'received response test.placeholders/Echo.EchoString = {\n'
            '      response: string = "hello world"\n'
            '    }', lines[3])

    def test_save_replay(self):
        save_path = tempfile.NamedTemporaryFile(suffix='_save.pb')
        fidlcat = Fidlcat(
            '--remote-component=echo_client.cm', '--to', save_path.name, 'run',
            ECHO_REALM_URL)
        self.assertEqual(fidlcat.wait(), 0, fidlcat.get_diagnose_msg())
        self.assertIn(
            'sent request test.placeholders/Echo.EchoString = {\n'
            '    value: string = "hello world"\n'
            '  }', fidlcat.stdout)

        fidlcat = Fidlcat('--from', save_path.name)
        self.assertEqual(fidlcat.wait(), 0, fidlcat.get_diagnose_msg())
        self.assertIn(
            'sent request test.placeholders/Echo.EchoString = {\n'
            '    value: string = "hello world"\n'
            '  }', fidlcat.stdout)

    def test_with_summary(self):
        fidlcat = Fidlcat(
            '--with=summary', '--from', TEST_DATA_DIR + '/echo.pb')
        self.assertEqual(fidlcat.wait(), 0, fidlcat.get_diagnose_msg())

        self.assertEqual(
            fidlcat.stdout, """\
--------------------------------------------------------------------------------echo_client.cm 1934080: 19 handles

  Process:eb13d6eb(proc-self)

  startup Thread:72a3d5c3(thread-self)

  startup Vmar:7ab3d41f(vmar-root)

  startup Channel:4eb3d2ab(dir:/svc)
      21320.052636 write request  fuchsia.io/Openable.Open

  startup Channel:8db3d62f(dir:/pkg)

  startup Channel:2f53d2bb(directory-request:/)
      21320.389674 read  request  fuchsia.io/Node1.Clone
    closed by zx_handle_close

  startup Clock:7343d0fb()

  startup Socket:1363d7e7(fd:1)
    closed by zx_handle_close

  startup Socket:0793d49b(fd:2)
    closed by zx_handle_close

  startup Job:0673d5f7(job-default)

  startup Vmo:6f43c91b(vdso-vmo)

  startup Vmo:6c83ca87(stack-vmo)

  Port:60a3d6c7(port:0)
    created by zx_port_create
    closed by zx_handle_close

  Timer:4863d187(timer:0)
    created by zx_timer_create
    closed by zx_handle_close

  Channel:c633d2db(channel:0)
    linked to Channel:fdd3d09f(channel:1)
    created by zx_channel_create
    closed by Channel:4eb3d2ab(dir:/svc) sending fuchsia.io/Openable.Open

  Channel:fdd3d09f(channel:1)
    linked to Channel:c633d2db(channel:0)
    created by zx_channel_create
      21320.136348 write request  fuchsia.io/Openable.Open
    closed by zx_handle_close

  Channel:7663d53b(channel:2)
    linked to Channel:7063d25b(channel:3)
    which is  Channel:60dc2bb3() in process echo_server.cm:1934409
    created by zx_channel_create
      21320.157131 write request  test.placeholders/Echo.EchoString
      21321.018177 read  response test.placeholders/Echo.EchoString
    closed by zx_handle_close

  Channel:7063d25b(channel:3)
    linked to Channel:7663d53b(channel:2)
    created by zx_channel_create
    closed by Channel:fdd3d09f(channel:1) sending fuchsia.io/Openable.Open

  Channel:31c3d79b()
    created by Channel:2f53d2bb(directory-request:/) receiving fuchsia.io/Node1.Clone
    closed by zx_handle_close

--------------------------------------------------------------------------------echo_server.cm 1934409: 18 handles

  Process:e19c2d4f(proc-self)

  startup Thread:4a8c356b(thread-self)

  startup Vmar:da0c2e4b(vmar-root)

  startup Channel:5e0c228f(dir:/svc)
      21320.611044 write request  fuchsia.io/Openable.Open

  startup Channel:c75c3537(dir:/pkg)

  startup Channel:dedc3503(directory-request:/)
      21320.679108 read  request  fuchsia.io/Node1.Clone
      21320.814595 read  request  fuchsia.io/Openable.Open

  startup Clock:8efc2deb()

  startup Socket:dcbc2b1b(fd:1)

  startup Socket:e3dc2843(fd:2)

  startup Job:d73c2ff7(job-default)

  startup Vmo:d10c3563(vdso-vmo)

  startup Vmo:db8c349f(stack-vmo)

  Port:c2cc378f(port:1)
    created by zx_port_create

  Timer:90bc2937(timer:1)
    created by zx_timer_create

  Channel:f2ec2f3b(channel:4)
    linked to Channel:d75c352b(channel:5)
    created by zx_channel_create
    closed by Channel:5e0c228f(dir:/svc) sending fuchsia.io/Openable.Open

  Channel:d75c352b(channel:5)
    linked to Channel:f2ec2f3b(channel:4)
    created by zx_channel_create

  Channel:aa3c2a07()
    created by Channel:dedc3503(directory-request:/) receiving fuchsia.io/Node1.Clone
    closed by zx_handle_close

  Channel:60dc2bb3()
    linked to Channel:7663d53b(channel:2) in process echo_client.cm:1934080
    created by Channel:dedc3503(directory-request:/) receiving fuchsia.io/Openable.Open
      21320.901025 read  request  test.placeholders/Echo.EchoString
      21320.992007 write response test.placeholders/Echo.EchoString
    closed by zx_handle_close
""")

    def test_with_top(self):
        fidlcat = Fidlcat('--with=top', '--from', TEST_DATA_DIR + '/echo.pb')
        self.assertEqual(fidlcat.wait(), 0, fidlcat.get_diagnose_msg())

        self.assertEqual(
            fidlcat.stdout, """\
--------------------------------------------------------------------------------echo_client.cm 1934080: 5 events
  fuchsia.io/Openable: 2 events
    Open: 2 events
      21320.052636 write request  fuchsia.io/Openable.Open(Channel:4eb3d2ab(dir:/svc))
      21320.136348 write request  fuchsia.io/Openable.Open(Channel:fdd3d09f(channel:1))

  test.placeholders/Echo: 2 events
    EchoString: 2 events
      21320.157131 write request  test.placeholders/Echo.EchoString(Channel:7663d53b(channel:2))
      21321.018177 read  response test.placeholders/Echo.EchoString(Channel:7663d53b(channel:2))

  fuchsia.io/Node1: 1 event
    Clone: 1 event
      21320.389674 read  request  fuchsia.io/Node1.Clone(Channel:2f53d2bb(directory-request:/))

--------------------------------------------------------------------------------echo_server.cm 1934409: 5 events
  fuchsia.io/Openable: 2 events
    Open: 2 events
      21320.611044 write request  fuchsia.io/Openable.Open(Channel:5e0c228f(dir:/svc))
      21320.814595 read  request  fuchsia.io/Openable.Open(Channel:dedc3503(directory-request:/))

  test.placeholders/Echo: 2 events
    EchoString: 2 events
      21320.901025 read  request  test.placeholders/Echo.EchoString(Channel:60dc2bb3())
      21320.992007 write response test.placeholders/Echo.EchoString(Channel:60dc2bb3())

  fuchsia.io/Node1: 1 event
    Clone: 1 event
      21320.679108 read  request  fuchsia.io/Node1.Clone(Channel:dedc3503(directory-request:/))
""")

    def test_with_top_and_unknown_message(self):
        fidlcat = Fidlcat(
            '--with=top', '--from', TEST_DATA_DIR + '/snapshot.pb')
        self.assertEqual(fidlcat.wait(), 0, fidlcat.get_diagnose_msg())

        self.assertIn(
            '  unknown interfaces: : 1 event\n'
            '      6862061079.791403 call   ordinal=36dadb5482dc1d55('
            'Channel:9b71d5c7(dir:/svc/fuchsia.feedback.DataProvider))\n',
            fidlcat.stdout)

    def test_with_messages_and_unknown_message(self):
        fidlcat = Fidlcat(
            '--messages=.*x.*', '--from', TEST_DATA_DIR + '/snapshot.pb')
        self.assertEqual(fidlcat.wait(), 0, fidlcat.get_diagnose_msg())

        # We only check that fidlcat didn't crash.
        self.assertIn(
            'Stop monitoring exceptions.cmx koid 19884\n', fidlcat.stdout)


if __name__ == '__main__':
    unittest.main()
