# Debug tests using zxdb

This pages provides details and examples related to using the Fuchsia
debugger (`zxdb`) with the [`fx test`][fx-test] command.

## Entering the debugger from fx test {:#entering-the-debugger-from-fx-test}

The `fx test` command supports the `--break-on-failure` and `--breakpoint`
flags, which allows you to debug tests using `zxdb`. If your test uses a
compatible test runner (that is, gTest, gUnit, or Rust today), adding the
`--break-on-failure` flag will cause test failures to pause test execution
and enter the `zxdb` debug environment, for example:

```none {:.devsite-disable-click-to-copy}
$ fx test --break-on-failure rust_crasher_test.cm

<...fx test startup...>

Running 1 tests

Starting: fuchsia-pkg://fuchsia.com/crasher_test#meta/rust_crasher_test.cm
Command: fx ffx test run --max-severity-logs WARN --break-on-failure fuchsia-pkg://fuchsia.com/crasher_test?hash=1cceb326c127e245f0052367142aee001f82a73c6f33091fe7999d43a94b1b34#meta/rust_crasher_test.cm

Status: [duration: 13.3s]  [tasks: 3 running, 15/18 complete]
  Running 1 tests                      [                                                                                                     ]           0.0%
⚠️  zxdb caught test failure in rust_crasher_test.cm, type `frame` to get started.
   14 LLVM_LIBC_FUNCTION(void, abort, ()) {
   15   for (;;) {
 ▶ 16     CRASH_WITH_UNIQUE_BACKTRACE();
   17     _zx_process_exit(ZX_TASK_RETCODE_EXCEPTION_KILL);
   18   }
══════════════════════════
 Invalid opcode exception
══════════════════════════
 Process 1 (koid=107752) thread 1 (koid=107754)
 Faulting instruction: 0x4159210ab797

🛑 process 1 __llvm_libc::__abort_impl__() • abort.cc:16
[zxdb]
```

From this point, you may use `zxdb` as normal. However, since the thread
is already in a fatal exception, typical execution commands such as `step`,
`next`, and `until` are not available. Inspection commands such as `print`,
`frame`, and `backtrace` are available for the duration of the debugging
session.

## Executing test cases in parallel {:#executing-test-cases-in-parallel}

Depending on the options given to `fx test` or test runner default
configurations, multiple test cases may fail in parallel. Supported test
runners all spawn an individual process for each test case, and the default
configuration may allow for multiple test processes to be running at the
same time.

When running with `fx test`, `zxdb` attaches to _all_ processes in your
test's realm. A test case failure will stop _only_ that particular process.

Parallel execution will only stop if and only if the number of test case
failures is equal to the number of allowed parallel test cases. Once any
process is detached from `zxdb`, another test case process will begin
immediately.

`zxdb` is designed to handle multi-process debugging. You can inspect the
current attached processes and their execution states with the `process` noun
or with the `status` command. The currently "active" process is marked with
a "▶" sign.

For more detailed information, see [Interaction model][interaction-model] (or
use the `help` command).

## Closing the debugger {:#closing-the-debugger}

After inspecting your test failure, you resume the test execution by detaching
from your test process, for example, using `kill`, `detach`, or `continue`.

As discussed in the previous section, multiple test cases may fail in parallel.
If you do not explicitly detach from all attached processes, `zxdb` remains in
the foreground. You can see all attached processes using the `process` noun.

You can also use certain commands to detach from all processes (for example,
`quit`, `detach *`, and `ctrl+d`) and resume execution of the test suite
immediately.

## Tutorial

This tutorial walks through a debugging workflow using the `fx test` command
and the Fuchsia debugger (`zxdb`).

### 1. Understand test cases {:#understand-test-cases}

Note: In most test cases, no additional setup is necessary in the code to run
the `fx test` command with `zxdb`.

* {Rust}

  Rust tests are executed by the [Rust test runner][rust-test-runner]. Unlike
  gTest or gUnit runners for C++ tests, the Rust test runner defaults to running
  test cases in parallel. This creates a different experience while using the
  `--break-on-failure` feature. See the section for debugging
  [parallel processes](#executing-test-casess-in-parallel) about expectations
  while debugging parallel test processes. This case is supported, and will
  function well with `zxdb`.

  Here's some sample [rust test code][archivist-test-example] (modified from
  the original), abbreviated for brevity:

  ```rust
  ...
  let mut log_helper2 = LogSinkHelper::new(&directory);
  log_helper2.write_log("my msg1");
  log_helper.write_log("my msg2");

  let mut expected = vec!["my msg1".to_owned(), "my msg3".to_owned()];
  expected.sort();
  let mut actual = vec![recv_logs.next().await.unwrap(), recv_logs.next().await.unwrap()];
  actual.sort();

  assert_eq!(expected, actual);
  ...
  ```

* {C++}

  The gTest test runner by default executes test cases serially, so only one
  test failure will be debugged at a time. Executing test cases in parallel
  is supported, and may be done by adding the `--parallel-cases` flag to the
  `fx test` command. Let's look at some sample
  [C++ test code][debug-agent-test-example] using gTest (abbreviated for
  brevity):

  ```cpp
  // Inject 1 process.
  auto process1 = std::make_unique<MockProcess>(nullptr, kProcessKoid1, kProcessName1);
  process1->AddThread(kProcess1ThreadKoid1);
  harness.debug_agent()->InjectProcessForTest(std::move(process1));

  // And another, with 2 threads.
  auto process2 = std::make_unique<MockProcess>(nullptr, kProcessKoid2, kProcessName2);
  process2->AddThread(kProcess2ThreadKoid1);
  process2->AddThread(kProcess2ThreadKoid2);
  harness.debug_agent()->InjectProcessForTest(std::move(process2));

  reply = {};
  remote_api->OnStatus(request, &reply);

  ASSERT_EQ(reply.processes.size(), 3u);  // <-- This will fail, since reply.processes.size() == 2
  EXPECT_EQ(reply.processes[0].process_koid, kProcessKoid1);
  EXPECT_EQ(reply.processes[0].process_name, kProcessName1);
  ...
  ```

### 2. Execute tests {:#execute-tests}

* {Rust}

  Execute the tests with the `fx test --break-on-failure` command, for example:

  ```none {:.devsite-disable-click-to-copy}
  $ fx test -o --break-on-failure archivist-unittests

  <...fx test startup...>

  Running 1 tests

  Starting: fuchsia-pkg://fuchsia.com/archivist-tests#meta/archivist-unittests.cm
  Command: fx ffx test run --max-severity-logs WARN --break-on-failure fuchsia-pkg://fuchsia.com/archivist-tests?hash=9a531e48fe82d86edef22f86f7e9b819d18a7d678f0823912d9224dd91f8926f#meta/archivist-unittests.cm
  Running test 'fuchsia-pkg://fuchsia.com/archivist-tests?hash=9a531e48fe82d86edef22f86f7e9b819d18a7d678f0823912d9224dd91f8926f#meta/archivist-unittests.cm'

  [RUNNING] archivist::tests::can_log_and_retrive_log
  [101430.272555][5631048][5631050][<root>][can_log_and_retrive_log] WARN: Failed to create event source for log sink requests err=Error connecting to protocol path: /events/log_sink_requested_event_stream

  Caused by:
      NOT_FOUND
  [101430.277339][5631048][5631050][<root>][can_log_and_retrive_log] WARN: Failed to create event source for InspectSink requests err=Error connecting to protocol path: /events/inspect_sink_requested_event_stream
  [101430.336160][5631048][5631050][<root>][can_log_and_retrive_log] INFO: archivist: Entering core loop.
  [101430.395986][5631048][5631050][<root>][can_log_and_retrive_log] ERROR: [src/lib/diagnostics/log/rust/src/lib.rs(62)] PANIC info=panicked at ../../src/diagnostics/archivist/src/archivist.rs:544:9:
  assertion `left == right` failed
    left: ["my msg1", "my msg2"]
   right: ["my msg1", "my msg3"]

  👋 zxdb is loading symbols to debug test failure in archivist-unittests.cm, please wait.
  ⚠️  test failure in archivist-unittests.cm, type `frame` or `help` to get started.
     11 namespace LIBC_NAMESPACE {
     12
   ▶ 13 LLVM_LIBC_FUNCTION(void, abort, ()) { CRASH_WITH_UNIQUE_BACKTRACE(); }
     14
     15 }  // namespace LIBC_NAMESPACE
  ══════════════════════════
   Invalid opcode exception
  ══════════════════════════
   Process 10 (koid=5495424) thread 1 (koid=5495428)
   Faulting instruction: 0x41a5680114d7

  🛑 process 10 __llvm_libc::__abort_impl__() • abort.cc:13
  [zxdb]
  ```

  Notice that the output from the test is mixed up, this is because
  the rust test runner runs test cases in
  [parallel by default][rust-test-runner-parallel-default]. You can avoid
  this by using this `--parallel-cases` option to `fx test`, for example:
  `fx test --parallel-cases 1 --break-on-failure archivist-unittests`

* {C++}

  Execute the tests with the `fx test --break-on-failure` command, for example:

  ```none {:.devsite-disable-click-to-copy}
  $ fx test -o --break-on-failure debug_agent_unit_tests

  <...fx test startup...>

  Starting: fuchsia-pkg://fuchsia.com/debug_agent_unit_tests#meta/debug_agent_unit_tests.cm (NOT HERMETIC)
  Command: fx ffx test run --realm /core/testing:system-tests --max-severity-logs WARN --break-on-failure fuchsia-pkg://fuchsia.com/debug_agent_unit_tests?hash=3f6d97801bb147034a344e3fe1bb69291a7b690b9d3d075246ddcba59397ac12#meta/debug_agent_unit_tests.cm

  Status: [duration: 30.9s]  [tasks: 3 running, 15/19 complete]
    Running 2 tests                      [                                                                                                     ]           0.0%
  ⚠️  zxdb caught test failure in debug_agent_unit_tests.cm, type `frame` to get started.
     5381      (defined(__x86_64__) || defined(__i386__)))
     5382       // with clang/gcc we can achieve the same effect on x86 by invoking int3
   ▶ 5383       asm("int3");
     5384 #elif GTEST_HAS_BUILTIN(__builtin_trap)
     5385       __builtin_trap();
  🛑 thread 1 testing::UnitTest::AddTestPartResult(testing::UnitTest*, testing::TestPartResult::Type, const char*, int, std::__2::string const&, std::__2::string const&) • gtest.cc:5383
  [zxdb]
  ```

### 3. Examine failures {:#examine-failures}

* {Rust}

  We caught a test failure, Rust tests issue an `abort` on failure, which `zxdb`
  notices and reports. We can view the code of the current frame with `list`,
  for example:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] list
      8
      9 #include "src/__support/common.h"
     10
     11 namespace LIBC_NAMESPACE {
     12
   ▶ 13 LLVM_LIBC_FUNCTION(void, abort, ()) { CRASH_WITH_UNIQUE_BACKTRACE(); }
     14
     15 }  // namespace LIBC_NAMESPACE
  [zxdb]
  ```

  But that's not the code we're interested in. When we look at a stack trace,
  we see code from our test is in frame #17:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] frame
  ▶ 0…12 «Rust library» (-r expands)
    13 std::panicking::begin_panic_handler(…) • library/std/src/panicking.rs:645
    14 core::panicking::panic_fmt(…) • library/core/src/panicking.rs:72
    15 core::panicking::assert_failed_inner(…) • library/core/src/panicking.rs:402
    16 core::panicking::assert_failed<…>(…) • /b/s/w/ir/x/w/fuchsia-third_party-rust/library/core/src/panicking.rs:357
    17 archivist_lib_lib_test::archivist::tests::can_log_and_retrive_log::test_entry_point::λ(…) • archivist.rs:544
    18 core::future::future::«impl»::poll<…>(…) • future/future.rs:123
    19 fuchsia_async::test_support::«impl»::run_singlethreaded::λ::λ(…) • test_support.rs:26
    20 fuchsia_async::test_support::«impl»::run_singlethreaded::λ::λ(…) • test_support.rs:121
    21 fuchsia_async::atomic_future::«impl»::poll<…>(…) • atomic_future.rs:78
    22 fuchsia_async::atomic_future::AtomicFuture::try_poll(…) • atomic_future.rs:223
    23 fuchsia_async::runtime::fuchsia::executor::common::Inner::try_poll(…) • executor/common.rs:588
    24 fuchsia_async::runtime::fuchsia::executor::common::Inner::poll_ready_tasks(…) • executor/common.rs:148
    25 fuchsia_async::runtime::fuchsia::executor::common::Inner::worker_lifecycle<…>(…) • executor/common.rs:448
    26 fuchsia_async::runtime::fuchsia::executor::local::LocalExecutor::run<…>(…) • executor/local.rs:100
    27 fuchsia_async::runtime::fuchsia::executor::local::LocalExecutor::run_singlethreaded<…>(…) • executor/local.rs:68
    28 fuchsia_async::test_support::«impl»::run_singlethreaded::λ() • test_support.rs:119
    29 fuchsia_async::test_support::Config::in_parallel(…) • test_support.rs:214
    30 fuchsia_async::test_support::«impl»::run_singlethreaded(…) • test_support.rs:116
    31 fuchsia_async::test_support::run_singlethreaded_test<…>(…) • test_support.rs:226
    32 fuchsia::test_singlethreaded<…>(…) • fuchsia/src/lib.rs:188
    33 archivist_lib_lib_test::archivist::tests::can_log_and_retrive_log() • archivist.rs:519
    34 archivist_lib_lib_test::archivist::tests::can_log_and_retrive_log::λ(…) • archivist.rs:520
    35 core::ops::function::FnOnce::call_once<…>(…) • /b/s/w/ir/x/w/fuchsia-third_party-rust/library/core/src/ops/function.rs:250
    36 core::ops::function::FnOnce::call_once<…>(…) • library/core/src/ops/function.rs:250 (inline)
    37 test::__rust_begin_short_backtrace<…>(…) • library/test/src/lib.rs:621
    38 test::run_test_in_spawned_subprocess(…) • library/test/src/lib.rs:749
    39 test::test_main_static_abort(…) • library/test/src/lib.rs:197
    40 archivist_lib_lib_test::main() • archivist/src/lib.rs:1
    41…58 «Rust startup» (-r expands)
  [zxdb]
  ```

  We can view our test's source code by using the `frame` noun with an index
  as a prefix to our `list` command, for example:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] frame 17 list
     539         expected.sort();
     540
     541         let mut actual = vec![recv_logs.next().await.unwrap(), recv_logs.next().await.unwrap()];
     542         actual.sort();
     543
   ▶ 544         assert_eq!(expected, actual);
     545
     546         // can log after killing log sink proxy
     547         log_helper.kill_log_sink();
     548         log_helper.write_log("my msg1");
     549         log_helper.write_log("my msg2");
     550
     551         assert_eq!(
     552             expected,
     553             vec! {recv_logs.next().await.unwrap(),recv_logs.next().await.unwrap()}
     554         );
  ```

  That's inconvenient, we have to type `frame 17` before every command to
  interact with the part of our code we're interested in. Notice the "▶"
  from the output of `frame`. It points to frame 0, indicating that it is
  the "active" frame. Let's select our frame as the "active" frame with
  just the `frame` noun with the frame's index from above so we can work
  directly with what we want to look at:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] frame 17
  archivist_lib_lib_test::archivist::tests::can_log_and_retrive_log::test_entry_point::λ(…) • archivist.rs:528
  [zxdb] frame
    0…12 «Rust library» (-r expands)
    13 std::panicking::begin_panic_handler(…) • library/std/src/panicking.rs:645
    14 core::panicking::panic_fmt(…) • library/core/src/panicking.rs:72
    15 core::panicking::assert_failed_inner(…) • library/core/src/panicking.rs:402
    16 core::panicking::assert_failed<…>(…) • /b/s/w/ir/x/w/fuchsia-third_party-rust/library/core/src/panicking.rs:357
  ▶ 17 archivist_lib_lib_test::archivist::tests::can_log_and_retrive_log::test_entry_point::λ(…) • archivist.rs:544
    18 core::future::future::«impl»::poll<…>(…) • future/future.rs:123
    19 fuchsia_async::test_support::«impl»::run_singlethreaded::λ::λ(…) • test_support.rs:26
    20 fuchsia_async::test_support::«impl»::run_singlethreaded::λ::λ(…) • test_support.rs:121
  ...
  ```

  Now, all commands we run will be in the context of frame #17. Let's list
  the source code again to be sure, with a little bit of additional context:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] list -c 10
     534         let mut log_helper2 = LogSinkHelper::new(&directory);
     535         log_helper2.write_log("my msg1");
     536         log_helper.write_log("my msg2");
     537
     538         let mut expected = vec!["my msg1".to_owned(), "my msg3".to_owned()];
     539         expected.sort();
     540
     541         let mut actual = vec![recv_logs.next().await.unwrap(), recv_logs.next().await.unwrap()];
     542         actual.sort();
     543
   ▶ 544         assert_eq!(expected, actual);
     545
     546         // can log after killing log sink proxy
     547         log_helper.kill_log_sink();
     548         log_helper.write_log("my msg1");
     549         log_helper.write_log("my msg2");
     550
     551         assert_eq!(
     552             expected,
     553             vec! {recv_logs.next().await.unwrap(),recv_logs.next().await.unwrap()}
     554         );
  ```

  Great! Now, why did the test fail? Let's print out some variables to see
  what's going on. We have a local variable in this frame, `actual`, which
  should have some strings that we added by calling `write_log` on our
  `log_helper` and `log_helper2` instances and by receiving them with the
  mpsc channel `recv_logs`:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] print expected
  vec!["my msg1", "my msg3"]
  [zxdb] print actual
  vec!["my msg1", "my msg2"]
  ```

  Aha, our test's expectation is slightly wrong. We expected `"my msg3"` to
  be the second string, but actually logged `"my msg2"`. We can correct the
  test to expect `"my msg2"`. We can now detach from our tests to continue
  and complete the test suite:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] quit

  <...fx test output continues...>

  Failed tests: archivist::tests::can_log_and_retrive_log
  122 out of 123 tests passed...

  Test fuchsia-pkg://fuchsia.com/archivist-tests?hash=8bcb30a2bfb923a4b42d1f0ea590af613ab0b1aa1ac67ada56ae4d325f3330a0#meta/archivist-unittests.cm produced unexpected high-severity logs:
  ----------------xxxxx----------------
  [105255.347070][5853309][5853311][<root>][can_log_and_retrive_log] ERROR: [src/lib/diagnostics/log/rust/src/lib.rs(62)] PANIC info=panicked at ../../src/diagnostics/archivist/src/archivist.rs:544:9:
  assertion `left == right` failed
    left: ["my msg1", "my msg2"]
   right: ["my msg1", "my msg3"]

  ----------------xxxxx----------------
  Failing this test. See: https://fuchsia.dev/fuchsia-src/development/diagnostics/test_and_logs#restricting_log_severity

  fuchsia-pkg://fuchsia.com/archivist-tests?hash=8bcb30a2bfb923a4b42d1f0ea590af613ab0b1aa1ac67ada56ae4d325f3330a0#meta/archivist-unittests.cm completed with result: FAILED
  The test was executed in the hermetic realm. If your test depends on system capabilities, pass in correct realm. See https://fuchsia.dev/go/components/non-hermetic-tests
  Tests failed.
  Deleting 1 files at /tmp/tmpgr0otc3w: ffx_logs/ffx.log
  To keep these files, set --ffx-output-directory.
  ```

  Now we can fix the test:

  ```diff
  - let mut expected = vec!["my msg1".to_owned(), "my msg3".to_owned()];
  + let mut expected = vec!["my msg1".to_owned(), "my msg2".to_owned()];
  ```

  and run the tests again:

  ```none {:.devsite-disable-click-to-copy}
  $ fx test --break-on-failure archivist-unittests

  <...fx test startup...>

  Running 1 tests

  Starting: fuchsia-pkg://fuchsia.com/archivist-tests#meta/archivist-unittests.cm
  Command: fx ffx test run --max-severity-logs WARN --break-on-failure fuchsia-pkg://fuchsia.com/archivist-tests?hash=454897cb1be6b88c2aeb4b5abf474894b629d30ca50f7dfaa23497fd3848a566#meta/archivist-unittests.cm
  Running test 'fuchsia-pkg://fuchsia.com/archivist-tests?hash=454897cb1be6b88c2aeb4b5abf474894b629d30ca50f7dfaa23497fd3848a566#meta/archivist-unittests.cm'
  [RUNNING] accessor::tests::accessor_skips_invalid_selectors
  [RUNNING] accessor::tests::batch_iterator_on_ready_is_called
  [RUNNING] accessor::tests::batch_iterator_terminates_on_client_disconnect
  [RUNNING] accessor::tests::buffered_iterator_handles_peer_closed
  [RUNNING] accessor::tests::buffered_iterator_handles_two_consecutive_buffer_waits
  [RUNNING] accessor::tests::logs_only_accept_basic_component_selectors
  [RUNNING] accessor::tests::socket_writer_does_not_handle_cbor
  [RUNNING] accessor::tests::socket_writer_handles_closed_socket
  [RUNNING] accessor::tests::socket_writer_handles_text
  [RUNNING] archivist::tests::can_log_and_retrive_log
  [PASSED]  accessor::tests::socket_writer_handles_text
  [RUNNING] archivist::tests::log_from_multiple_sock
  [PASSED]  accessor::tests::buffered_iterator_handles_two_consecutive_buffer_waits
  <...lots of tests...>
  [RUNNING] logs::tests::unfiltered_stats
  [PASSED]  logs::tests::test_debuglog_drainer
  [RUNNING] utils::tests::drop_test
  [PASSED]  logs::tests::test_filter_by_pid
  [PASSED]  logs::tests::test_filter_by_min_severity
  [PASSED]  logs::tests::test_filter_by_tags
  [PASSED]  logs::tests::test_filter_by_tid
  [PASSED]  logs::tests::test_log_manager_dump
  [PASSED]  logs::tests::test_log_manager_simple
  [PASSED]  logs::tests::unfiltered_stats
  [PASSED]  logs::tests::test_structured_log
  [PASSED]  logs::tests::attributed_inspect_two_mixed_streams_different_identities
  [PASSED]  utils::tests::drop_test

  123 out of 123 tests passed...
  fuchsia-pkg://fuchsia.com/archivist-tests?hash=454897cb1be6b88c2aeb4b5abf474894b629d30ca50f7dfaa23497fd3848a566#meta/archivist-unittests.cm completed with result: PASSED
  Deleting 1 files at /tmp/tmpho9yjjz9: ffx_logs/ffx.log
  To keep these files, set --ffx-output-directory.

  Status: [duration: 36.4s] [tests: PASS: 1 FAIL: 0 SKIP: 0]
    Running 1 tests                            [====================================================================================================================]            100.0%
  ```

* {C++}

  We caught a test failure, gTest has an option to insert a software
  breakpoint in the path of a test failure, which is inlined into our
  test. We can view the code of the current frame with `list`, for
  example:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] list
  ...
     5381      (defined(__x86_64__) || defined(__i386__)))
     5382       // with clang/gcc we can achieve the same effect on x86 by invoking int3
   ▶ 5383       asm("int3");
     5384 #elif GTEST_HAS_BUILTIN(__builtin_trap)
     5385       __builtin_trap();
  ...
  ```

  But that's not the code we're interested in. When we look at a stack
  trace, we see code from our test is in frame #2:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] frame
  ▶ 0 testing::UnitTest::AddTestPartResult(…) • gtest.cc:5383
    1 testing::internal::AssertHelper::operator=(…) • gtest.cc:476
    2 debug_agent::DebugAgentTests_OnGlobalStatus_Test::TestBody(…) • debug_agent_unittest.cc:105 <-- This is the test's source code.
    3 testing::internal::HandleSehExceptionsInMethodIfSupported<…>(…) • gtest.cc:2635
    4 testing::internal::HandleExceptionsInMethodIfSupported<…>(…) • gtest.cc:2690
    5 testing::Test::Run(…) • gtest.cc:2710
    6 testing::TestInfo::Run(…) • gtest.cc:2859
    7 testing::TestSuite::Run(…) • gtest.cc:3038
    8 testing::internal::UnitTestImpl::RunAllTests(…) • gtest.cc:5942
    9 testing::internal::HandleSehExceptionsInMethodIfSupported<…>(…) • gtest.cc:2635
    10 testing::internal::HandleExceptionsInMethodIfSupported<…>(…) • gtest.cc:2690
    11 testing::UnitTest::Run(…) • gtest.cc:5506
    12 RUN_ALL_TESTS() • gtest.h:2318
    13 main(…) • run_all_unittests.cc:20
    14…17 «libc startup» (-r expands)
  [zxdb]
  ```

  We can view our test's source code by using the `frame` noun with
  an index as a prefix to our `list` command, for example:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] frame 2 list
     100   harness.debug_agent()->InjectProcessForTest(std::move(process2));
     101
     102   reply = {};
     103   remote_api->OnStatus(request, &reply);
     104
   ▶ 105   ASSERT_EQ(reply.processes.size(), 3u); <-- This assertion failed.
     106   EXPECT_EQ(reply.processes[0].process_koid, kProcessKoid1);
     107   EXPECT_EQ(reply.processes[0].process_name, kProcessName1);
     108   ASSERT_EQ(reply.processes[0].threads.size(), 1u);
     109   EXPECT_EQ(reply.processes[0].threads[0].id.process, kProcessKoid1);
     110   EXPECT_EQ(reply.processes[0].threads[0].id.thread, kProcess1ThreadKoid1);
     111
     112   EXPECT_EQ(reply.processes[1].process_koid, kProcessKoid2);
     113   EXPECT_EQ(reply.processes[1].process_name, kProcessName2);
     114   ASSERT_EQ(reply.processes[1].threads.size(), 2u);
     115   EXPECT_EQ(reply.processes[1].threads[0].id.process, kProcessKoid2);
  ```

  That's inconvenient, we have to type `frame 2` before every command
  to interact with the part of our code we're interested in. Notice
  the "▶" from the output of `frame`. It points to frame 0, indicating
  that it is the "active" frame. Let's select our frame as the "active"
  frame with just the `frame` noun with the frame's index from above so
  we can work directly with what we want to look at:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] frame 2
  debug_agent::DebugAgentTests_OnGlobalStatus_Test::TestBody(…) • debug_agent_unittest.cc:105
  [zxdb] frame
    0 testing::UnitTest::AddTestPartResult(…) • gtest.cc:5383
    1 testing::internal::AssertHelper::operator=(…) • gtest.cc:476
  ▶ 2 debug_agent::DebugAgentTests_OnGlobalStatus_Test::TestBody(…) • debug_agent_unittest.cc:105
    3 testing::internal::HandleSehExceptionsInMethodIfSupported<…>(…) • gtest.cc:2635
    4 testing::internal::HandleExceptionsInMethodIfSupported<…>(…) • gtest.cc:2690
    5 testing::Test::Run(…) • gtest.cc:2710
    6 testing::TestInfo::Run(…) • gtest.cc:2859
    7 testing::TestSuite::Run(…) • gtest.cc:3038
    8 testing::internal::UnitTestImpl::RunAllTests(…) • gtest.cc:5942
    9 testing::internal::HandleSehExceptionsInMethodIfSupported<…>(…) • gtest.cc:2635
    10 testing::internal::HandleExceptionsInMethodIfSupported<…>(…) • gtest.cc:2690
    11 testing::UnitTest::Run(…) • gtest.cc:5506
    12 RUN_ALL_TESTS() • gtest.h:2318
    13 main(…) • run_all_unittests.cc:20
    14…17 «libc startup» (-r expands)
  ```

  Now, all commands we run will be in the context of frame #2. Let's
  list the source code again to be sure:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] list
     100   harness.debug_agent()->InjectProcessForTest(std::move(process2));
     101
     102   reply = {};
     103   remote_api->OnStatus(request, &reply);
     104
   ▶ 105   ASSERT_EQ(reply.processes.size(), 3u);
     106   EXPECT_EQ(reply.processes[0].process_koid, kProcessKoid1);
     107   EXPECT_EQ(reply.processes[0].process_name, kProcessName1);
     108   ASSERT_EQ(reply.processes[0].threads.size(), 1u);
     109   EXPECT_EQ(reply.processes[0].threads[0].id.process, kProcessKoid1);
     110   EXPECT_EQ(reply.processes[0].threads[0].id.thread, kProcess1ThreadKoid1);
     111
     112   EXPECT_EQ(reply.processes[1].process_koid, kProcessKoid2);
     113   EXPECT_EQ(reply.processes[1].process_name, kProcessName2);
     114   ASSERT_EQ(reply.processes[1].threads.size(), 2u);
     115   EXPECT_EQ(reply.processes[1].threads[0].id.process, kProcessKoid2);
  ```

  Cool! Now, why did the test fail? Let's print out some variables to
  see what's going on. We have a local variable in this frame, `reply`,
  which should have been populated by the function call to
  `remote_api->OnStatus`:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] print reply
  {
    processes = {
      [0] = {
        process_koid = 4660
        process_name = "process-1"
        components = {}
        threads = {
          [0] = {
            id = {process = 4660, thread = 1}
            name = "test thread"
            state = kRunning
            blocked_reason = kNotBlocked
            stack_amount = kNone
            frames = {}
          }
        }
      }
      [1] = {
        process_koid = 22136
        process_name = "process-2"
        components = {}
        threads = {
          [0] = {
            id = {process = 22136, thread = 1}
            name = "test thread"
            state = kRunning
            blocked_reason = kNotBlocked
            stack_amount = kNone
            frames = {}
          }
          [1] = {
            id = {process = 22136, thread = 2}
            name = "test thread"
            state = kRunning
            blocked_reason = kNotBlocked
            stack_amount = kNone
            frames = {}
          }
        }
      }
    }
    limbo = {}
    breakpoints = {}
    filters = {}
  }
  ```

  Okay, so the `reply` variable has been filled in with some
  information, the expectation is that the size of the `processes`
  vector should be equal to 3. Let's just print that member variable
  of `reply` to get a clearer picture. We can also print the size
  method of that vector (general function calling support is not
  implemented yet):

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] print reply.processes
  {
    [0] = {
      process_koid = 4660
      process_name = "process-1"
      components = {}
      threads = {
        [0] = {
          id = {process = 4660, thread = 1}
          name = "test thread"
          state = kRunning
          blocked_reason = kNotBlocked
          stack_amount = kNone
          frames = {}
        }
      }
    }
    [1] = {
      process_koid = 22136
      process_name = "process-2"
      components = {}
      threads = {
        [0] = {
          id = {process = 22136, thread = 1}
          name = "test thread"
          state = kRunning
          blocked_reason = kNotBlocked
          stack_amount = kNone
          frames = {}
        }
        [1] = {
          id = {process = 22136, thread = 2}
          name = "test thread"
          state = kRunning
          blocked_reason = kNotBlocked
          stack_amount = kNone
          frames = {}
        }
      }
    }
  }
  [zxdb] print reply.processes.size()
  2
  ```

  Aha, so the test expectation is wrong, we only injected 2 mock
  processes in our test, but expected there to be 3. The test
  simply needs to be updated to expect the size of the
  `reply.processes` vector to be 2 instead of 3. We can close the
  debugger now to finish up the tests and then fix our test:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] quit

  <...fx test output continues...>

  Failed tests: DebugAgentTests.OnGlobalStatus <-- Failed test that we debugged.
  175 out of 176 attempted tests passed, 2 tests skipped...
  fuchsia-pkg://fuchsia.com/debug_agent_unit_tests?hash=3f6d97801bb147034a344e3fe1bb69291a7b690b9d3d075246ddcba59397ac12#meta/debug_agent_unit_tests.cm completed with result: FAILED
  Tests failed.


  FAILED: fuchsia-pkg://fuchsia.com/debug_agent_unit_tests#meta/debug_agent_unit_tests.cm
  ```

  Now that the source of the test failure was found, we can fix the test:

  ```diff
  -ASSERT_EQ(reply.processes.size(), 3u)
  +ASSERT_EQ(reply.processes.size(), 2u)
  ```

  and run `fx test` again:

  ```none {:.devsite-disable-click-to-copy}
  $ fx test --break-on-failure debug_agent_unit_tests

  You are using the new fx test, which is currently ready for general use ✅
  See details here: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/scripts/fxtest/rewrite
  To go back to the old fx test, use `fx --enable=legacy_fxtest test`, and please file a bug under b/293917801.

  Default flags loaded from /usr/local/google/home/jruthe/.fxtestrc:
  []

  Logging all output to: /usr/local/google/home/jruthe/upstream/fuchsia/out/workbench_eng.x64/fxtest-2024-03-25T15:56:31.874893.log.json.gz
  Use the `--logpath` argument to specify a log location or `--no-log` to disable

  🛑 Debugger integration is currently experimental, follow https://fxbug.dev/319320287 for updates 🛑
  To show all output, specify the `-o/--output` flag.

  Found 913 total tests in //out/workbench_eng.x64/tests.json

  Plan to run 1 test

  Refreshing 1 target
  > fx build src/developer/debug/debug_agent:debug_agent_unit_tests host_x64/debug_agent_unit_tests
  Use --no-build to skip building

  Executing build. Status output suspended.
  ninja: Entering directory `/usr/local/google/home/jruthe/upstream/fuchsia/out/workbench_eng.x64'
  [22/22](0) STAMP obj/src/developer/debug/debug_agent/debug_agent_unit_tests.stamp

  Running 1 test

  Starting: fuchsia-pkg://fuchsia.com/debug_agent_unit_tests#meta/debug_agent_unit_tests.cm (NOT HERMETIC)
  Command: fx ffx test run --realm /core/testing:system-tests --max-severity-logs WARN --break-on-failure fuchsia-pkg://fuchsia.com/debug_agent_unit_tests?hash=399ff8d9871a6f0d53557c3d7c233cad645061016d44a7855dcea2c7b8af8101#meta/debug_agent_unit_tests.cm
  Deleting 1 files at /tmp/tmp8m56ht95: ffx_logs/ffx.log
  To keep these files, set --ffx-output-directory.

  PASSED: fuchsia-pkg://fuchsia.com/debug_agent_unit_tests#meta/debug_agent_unit_tests.cm

  Status: [duration: 16.9s] [tests: PASS: 1 FAIL: 0 SKIP: 0]
    Running 1 tests                      [=====================================================================================================]         100.0%
  ```

  The debugger no longer appears, because we don't have any other
  test failures! Woohoo \o/

<!-- Reference links -->

[archivist-test-example]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/diagnostics/archivist/src/archivist.rs;l=539-549;drc=b266522960501274fbe62ac9a0d0f9631a2a58b6
[debug-agent-test-example]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/developer/debug/debug_agent/debug_agent_unittest.cc;l=63-147
[fx-test]: /docs/reference/testing/fx-test.md
[interaction-model]: /docs/development/debugger/commands.md
[rust-test-runner]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/sys/test_runners/rust/
[rust-test-runner-parallel-default]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/sys/test_runners/rust/src/test_server.rs;l=55
