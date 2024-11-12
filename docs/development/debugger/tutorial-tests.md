# Tutorial: Debug tests using zxdb

This tutorial walks through a debugging workflow using the `fx test` command
and the Fuchsia debugger (`zxdb`).

For additional info on debugging tests using zxdb, see
[Debug tests using zxdb][zxdb-test-doc]

## Understanding test cases {:#understand-test-cases .numbered}

Note: In most test cases, no additional setup is necessary in the code to run
the `fx test` command with `zxdb`.

* {Rust}

  Rust tests are executed by the [Rust test runner][rust-test-runner]. Unlike
  gTest or gUnit runners for C++ tests, the Rust test runner defaults to running
  test cases in parallel. This creates a different experience while using the
  `--break-on-failure` feature. For more information on expectations while
  debugging parallel test processes, see
  [Executing test cases in parallel][zxdb-parallel-tests]. Parallel test
  processes are supported.

  Here is an example based on some sample
  [rust test code][archivist-test-example]:

  Note: This code is a modified version of the original code and is abbreviated
  for brevity.

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

  You can add this test target to your build graph with `fx set`:

  ```posix-terminal
  fx set workbench_eng.x64 --with-tests //src/diagnostics/archivist:tests
  ```

* {C++}

  By default, the gTest test runner executes test cases serially, so only one
  test failure is debugged at a time. You can execute test cases in parallel
  by adding the `--parallel-cases` flag to the `fx test` command.

  Here is an example based on some sample
  [C++ test code][debug-agent-test-example]:

  Note: This code is abbreviated for brevity.

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

  You can add this test target to your build graph with `fx set`:

  ```posix-terminal
  fx set workbench_eng.x64 --with-tests //src/developer/debug:tests
  ```

## Executing tests {:#execute-tests .numbered}

* {Rust}

  Execute the tests with the `fx test --break-on-failure` command, for example:

  ```posix-terminal
  fx test -o --break-on-failure archivist-unittests
  ```

  The output looks like:

  ```none {:.devsite-disable-click-to-copy}
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
    536         actual.sort();
    537
  ▶ 538         assert_eq!(expected, actual);
    539
    540         // can log after killing log sink proxy
  🛑 process 9 archivist_lib_lib_test::archivist::tests::can_log_and_retrive_log::test_entry_point::λ(core::task::wake::Context*) • archivist.rs:538
  [zxdb]
  ```

  Notice that the output from the test is mixed up, this is because
  the rust test runner runs test cases in
  [parallel by default][rust-test-runner-parallel-default]. You can avoid
  this by using the `--parallel-cases` option with `fx test`, for example:
  `fx test --parallel-cases 1 --break-on-failure archivist-unittests`

* {C++}

  Execute the tests with the `fx test --break-on-failure` command, for example:

  ```posix-terminal
  fx test -o --break-on-failure debug_agent_unit_tests
  ```

  The output looks like:

  ```none {:.devsite-disable-click-to-copy}
  <...fx test startup...>

  Starting: fuchsia-pkg://fuchsia.com/debug_agent_unit_tests#meta/debug_agent_unit_tests.cm (NOT HERMETIC)
  Command: fx ffx test run --realm /core/testing:system-tests --max-severity-logs WARN --break-on-failure fuchsia-pkg://fuchsia.com/debug_agent_unit_tests?hash=3f6d97801bb147034a344e3fe1bb69291a7b690b9d3d075246ddcba59397ac12#meta/debug_agent_unit_tests.cm

  Status: [duration: 30.9s]  [tasks: 3 running, 15/19 complete]
    Running 2 tests                      [                                                                                                     ]           0.0%
  👋 zxdb is loading symbols to debug test failure in debug_agent_unit_tests.cm, please wait.
  ⚠️  test failure in debug_agent_unit_tests.cm, type `frame` or `help` to get started.
    103   remote_api->OnStatus(request, &reply);
    104
  ▶ 105   ASSERT_EQ(reply.processes.size(), 3u);
    106   EXPECT_EQ(reply.processes[0].process_koid, kProcessKoid1);
    107   EXPECT_EQ(reply.processes[0].process_name, kProcessName1);
  🛑 thread 1 debug_agent::DebugAgentTests_OnGlobalStatus_Test::TestBody(debug_agent::DebugAgentTests_OnGlobalStatus_Test*) • debug_agent_unittest.cc:105
  [zxdb]
  ```

## Examining failures {:#examine-failures .numbered}

* {Rust}

  The example contains a test failure, so Rust tests issue an `abort` on
  failure, which `zxdb` notices and reports. `zxdb` also analyzes the call
  stack from the abort and conveniently drop us straight into the source code
  that failed. You can view additional lines of the code from the current frame
  with `list`, for example:

  ```none {: .devsite-terminal data-terminal-prefix="[zxdb]" }
  list
    533         expected.sort();
    534
    535         let mut actual = vec![recv_logs.next().await.unwrap(), recv_logs.next().await.unwrap()];
    536         actual.sort();
    537
  ▶ 538         assert_eq!(expected, actual);
    539
    540         // can log after killing log sink proxy
    541         log_helper.kill_log_sink();
    542         log_helper.write_log("my msg1");
    543         log_helper.write_log("my msg2");
    544
    545         assert_eq!(
    546             expected,
    547             vec! {recv_logs.next().await.unwrap(),recv_logs.next().await.unwrap()}
    548         );
  [zxdb]
  ```

  You can also examine the entire call stack with `frame`, for example:

  ```none {: .devsite-terminal data-terminal-prefix="[zxdb]" }
  frame
    0…12 «Rust library» (-r expands)
    13 std::panicking::begin_panic_handler(…) • library/std/src/panicking.rs:645
    14 core::panicking::panic_fmt(…) • library/core/src/panicking.rs:72
    15 core::panicking::assert_failed_inner(…) • library/core/src/panicking.rs:402
    16 core::panicking::assert_failed<…>(…) • /b/s/w/ir/x/w/fuchsia-third_party-rust/library/core/src/panicking.rs:357
  ▶ 17 archivist_lib_lib_test::archivist::tests::can_log_and_retrive_log::test_entry_point::λ(…) • archivist.rs:544
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

  Or, when in an asynchronous context, you can use `async-backtrace`, for example:

  ```none {:.devsite-disable-click-to-copy}
  [zxdb] async-backtrace
  Task(id = 0)
  └─ fuchsia_async::test_support::«impl»::run_singlethreaded::λ • test_support.rs:122
     └─ fuchsia_async::test_support::«impl»::run_singlethreaded::λ • test_support.rs:27
        └─ archivist_lib_lib_test::archivist::tests::can_log_and_retrive_log::test_entry_point • archivist.rs:546
           └─ futures_util::stream::stream::next::Next
  Task(id = 1)
  └─ diagnostics_log::fuchsia::filter::«impl»::listen_to_interest_changes • fuchsia/filter.rs:77
     └─ fidl::client::QueryResponseFut
  Task(id = 2)
  └─ archivist_lib_lib_test::logs::repository::«impl»::new • logs/repository.rs:433
     └─ futures_util::stream::stream::next::Next
  Task(id = 3)
  └─ archivist_lib_lib_test::logs::repository::«impl»::process_removal_of_components • logs/repository.rs:232
     └─ futures_util::stream::stream::next::Next
  Task(id = 4)
  └─ archivist_lib_lib_test::logs::servers::log_settings::«impl»::new • log_settings.rs:33
     └─ futures_util::stream::stream::for_each_concurrent::ForEachConcurrent
  Task(id = 5)
  └─ archivist_lib_lib_test::inspect::servers::inspect_sink::«impl»::new • inspect_sink.rs:42
     └─ futures_util::stream::stream::for_each_concurrent::ForEachConcurrent
  Task(id = 6)
  └─ archivist_lib_lib_test::logs::servers::log_settings::«impl»::new • log_settings.rs:33
     └─ futures_util::stream::stream::for_each_concurrent::ForEachConcurrent
  Task(id = 7)
  └─ archivist_lib_lib_test::logs::servers::log_settings::«impl»::new • log_settings.rs:33
     └─ futures_util::stream::stream::for_each_concurrent::ForEachConcurrent
  Task(id = 8)
  └─ archivist_lib_lib_test::logs::servers::log_settings::«impl»::new • log_settings.rs:33
     └─ futures_util::stream::stream::for_each_concurrent::ForEachConcurrent
  Task(id = 9)
  └─ archivist_lib_lib_test::archivist::tests::run_archivist::λ • archivist.rs:518
     └─ archivist_lib_lib_test::archivist::«impl»::run • archivist.rs:342
        └─ futures_util::future::join::Join3
  Task(id = 10)
  └─ archivist_lib_lib_test::logs::testing::start_listener • logs/testing.rs:592
     └─ fuchsia_syslog_listener::run_log_listener_with_proxy • syslog-listener/src/lib.rs:78
        └─ fuchsia_syslog_listener::log_listener • syslog-listener/src/lib.rs:34
           └─ futures_util::stream::try_stream::try_next::TryNext
  Task(id = 11)
  └─ inspect_runtime::service::spawn_tree_server_with_stream • runtime/rust/src/service.rs:80
     └─ inspect_runtime::service::handle_request_stream • runtime/rust/src/service.rs:28
        └─ futures_util::stream::try_stream::try_next::TryNext
  Task(id = 12)
  └─ archivist_lib_lib_test::archivist::«impl»::run::λ • archivist.rs:293
     └─ archivist_lib_lib_test::events::router::«impl»::start • router.rs:96
        └─ futures_util::stream::stream::next::Next
  Task(id = 14)
  └─ archivist_lib_lib_test::logs::servers::log::«impl»::spawn • log.rs:46
     └─ archivist_lib_lib_test::logs::servers::log::«impl»::handle_requests • log.rs:78
        └─ futures_util::stream::stream::next::Next
  Task(id = 15)
  └─ archivist_lib_lib_test::logs::listener::«impl»::spawn • logs/listener.rs:83
     └─ archivist_lib_lib_test::logs::listener::«impl»::run • logs/listener.rs:106
        └─ archivist_lib_lib_test::logs::listener::«impl»::send_new_logs • logs/listener.rs:123
           └─ archivist_lib_lib_test::logs::listener::«impl»::send_log • logs/listener.rs:181
              └─ fidl::client::QueryResponseFut
  Task(id = 16)
  └─ archivist_lib_lib_test::logs::container::«impl»::actually_handle_log_sink • logs/container.rs:256
     └─ futures_util::stream::stream::next::Next
  Task(id = 17)
  └─ archivist_lib_lib_test::logs::container::«impl»::drain_messages • logs/container.rs:373
     └─ futures_util::stream::stream::next::Next
  Task(id = 18)
  └─ archivist_lib_lib_test::logs::container::«impl»::actually_handle_log_sink • logs/container.rs:256
     └─ futures_util::stream::stream::next::Next
  Task(id = 19)
  └─ archivist_lib_lib_test::logs::container::«impl»::drain_messages • logs/container.rs:373
     └─ futures_util::stream::stream::next::Next
  Scope(0x1771eceb2a0)
  └─ Task(id = 13)
     └─ vfs::execution_scope::«impl»::spawn • execution_scope.rs:120
        └─ core::future::poll_fn::PollFn
  ```

  All commands that you run are in the context of frame #17, as indicated by `▶`.
  You can list the source code again with a little bit of additional context:

  ```none {: .devsite-terminal data-terminal-prefix="[zxdb]" }
  list -c 10
    528         let mut log_helper2 = LogSinkHelper::new(&directory);
    529         log_helper2.write_log("my msg1");
    530         log_helper.write_log("my msg3");
    531
    532         let mut expected = vec!["my msg1".to_owned(), "my msg2".to_owned()];
    533         expected.sort();
    534
    535         let mut actual = vec![recv_logs.next().await.unwrap(), recv_logs.next().await.unwrap()];
    536         actual.sort();
    537
  ▶ 538         assert_eq!(expected, actual);
    539
    540         // can log after killing log sink proxy
    541         log_helper.kill_log_sink();
    542         log_helper.write_log("my msg1");
    543         log_helper.write_log("my msg2");
    544
    545         assert_eq!(
    546             expected,
    547             vec! {recv_logs.next().await.unwrap(),recv_logs.next().await.unwrap()}
    548         );
  [zxdb]
  ```

  To find out why the test failed, print out some variables to see what is
  happening. The `actual` frame contains a local variable, which
  should have some strings that were added by calling `write_log` on the
  `log_helper` and `log_helper2` instances and by receiving them with the
  mpsc channel `recv_logs`:

  ```none {: .devsite-terminal data-terminal-prefix="[zxdb]" }
  print expected
  vec!["my msg1", "my msg3"]
  [zxdb] print actual
  vec!["my msg1", "my msg2"]
  ```

  It seems that the test's expectations are slightly incorrect. It was expected
  that `"my msg3"` should be the second string, but it actually logged
  `"my msg2"`. You can correct the test to expect `"my msg2"`. You can now
  detach from the tests to continue and complete the test suite:

  ```none {: .devsite-terminal data-terminal-prefix="[zxdb]" }
  quit

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

  Now you can fix the test by making the following change to the code:

  Note: `-` indicates a removal of a line and `+` indicates an added line.

  ```diff
  - let mut expected = vec!["my msg1".to_owned(), "my msg3".to_owned()];
  + let mut expected = vec!["my msg1".to_owned(), "my msg2".to_owned()];
  ```

  You can now run the tests again:

  ```posix-terminal
  fx test --break-on-failure archivist-unittests
  ```

  The output should look like:

  ```none {:.devsite-disable-click-to-copy}
  <...fx test startup...>

  Running 1 tests

  Starting: fuchsia-pkg://fuchsia.com/archivist-tests#meta/archivist-unittests.cm
  Command: fx ffx test run --max-severity-logs WARN --break-on-failure fuchsia-pkg://fuchsia.com/archivist-tests?hash=36bf634de9f8850fad02fe43ec7fbe2b086000d0f55f7028d6d9fc8320738301#meta/archivist-unittests.cm
  Running test 'fuchsia-pkg://fuchsia.com/archivist-tests?hash=36bf634de9f8850fad02fe43ec7fbe2b086000d0f55f7028d6d9fc8320738301#meta/archivist-unittests.cm'
  [RUNNING]	accessor::tests::accessor_skips_invalid_selectors
  [RUNNING]	accessor::tests::batch_iterator_on_ready_is_called
  [RUNNING]	accessor::tests::batch_iterator_terminates_on_client_disconnect
  [RUNNING]	accessor::tests::buffered_iterator_handles_peer_closed
  [RUNNING]	accessor::tests::buffered_iterator_handles_two_consecutive_buffer_waits
  [RUNNING]	accessor::tests::logs_only_accept_basic_component_selectors
  [RUNNING]	accessor::tests::socket_writer_does_not_handle_cbor
  [RUNNING]	accessor::tests::socket_writer_handles_closed_socket
  [RUNNING]	accessor::tests::socket_writer_handles_text
  [RUNNING]	archivist::tests::can_log_and_retrive_log
  [PASSED]	accessor::tests::socket_writer_handles_text
  [RUNNING]	archivist::tests::log_from_multiple_sock
  [PASSED]	accessor::tests::socket_writer_does_not_handle_cbor
  [RUNNING]	archivist::tests::remote_log_test
  [PASSED]	accessor::tests::socket_writer_handles_closed_socket
  [RUNNING]	archivist::tests::stop_works
  [PASSED]	accessor::tests::buffered_iterator_handles_peer_closed
  [RUNNING]	configs::tests::parse_allow_empty_pipeline
  [PASSED]	accessor::tests::buffered_iterator_handles_two_consecutive_buffer_waits
  [RUNNING]	configs::tests::parse_disabled_valid_pipeline
  <...lots of tests...>
  [PASSED]	logs::repository::tests::multiplexer_broker_cleanup
  [PASSED]	logs::tests::attributed_inspect_two_v2_streams_different_identities
  [RUNNING]	logs::tests::unfiltered_stats
  [PASSED]	logs::tests::test_debuglog_drainer
  [RUNNING]	utils::tests::drop_test
  [PASSED]	logs::tests::test_filter_by_combination
  [PASSED]	logs::tests::test_filter_by_min_severity
  [PASSED]	logs::tests::test_filter_by_pid
  [PASSED]	logs::tests::test_filter_by_tags
  [PASSED]	logs::tests::test_filter_by_tid
  [PASSED]	logs::tests::test_log_manager_dump
  [PASSED]	logs::tests::test_structured_log
  [PASSED]	logs::tests::test_log_manager_simple
  [PASSED]	logs::tests::unfiltered_stats
  [PASSED]	utils::tests::drop_test

  128 out of 128 tests passed...
  fuchsia-pkg://fuchsia.com/archivist-tests?hash=36bf634de9f8850fad02fe43ec7fbe2b086000d0f55f7028d6d9fc8320738301#meta/archivist-unittests.cm completed with result: PASSED
  Deleting 1 files at /tmp/tmpho9yjjz9: ffx_logs/ffx.log
  To keep these files, set --ffx-output-directory.

  Status: [duration: 36.4s] [tests: PASS: 1 FAIL: 0 SKIP: 0]
    Running 1 tests                            [====================================================================================================================]            100.0%
  ```

* {C++}

  The example contains a test failure, gTest has an option to insert a software
  breakpoint in the path of a test failure, which `zxdb` picked up. `zxdb` has
  also determined the location of your test code based on this, and jumps
  straight to the frame from your test. You can view additional lines of code of
  the current frame with `list`, for example:

  ```none {: .devsite-terminal data-terminal-prefix="[zxdb]" }
  list
    100   harness.debug_agent()->InjectProcessForTest(std::move(process2));
    101
    102   reply = {};
    103   remote_api->OnStatus(request, &reply);
    104
  ▶ 105   ASSERT_EQ(reply.processes.size(), 3u);
    106   EXPECT_EQ(reply.processes[0].process_koid, kProcessKoid1);
    107   EXPECT_EQ(reply.processes[0].process_name, kProcessName1);
    108   ASSERT_EQ(reply.processes[0].threads.size(), 1u);
  ```

  You can see more lines of source code by using `list`'s `-c` flag:

  ```none {: .devsite-terminal data-terminal-prefix="[zxdb]" }
  list -c 10
      95   constexpr uint64_t kProcess2ThreadKoid2 = 0x2;
      96
      97   auto process2 = std::make_unique<MockProcess>(nullptr, kProcessKoid2, kProcessName2);
      98   process2->AddThread(kProcess2ThreadKoid1);
      99   process2->AddThread(kProcess2ThreadKoid2);
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
  [zxdb]
  ```

  You can also examine the full stack trace with the `frame` command:

  ```none {: .devsite-terminal data-terminal-prefix="[zxdb]" }
  frame
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
  [zxdb]
  ```

  Notice that the `▶` points to your test's source code frame, indicating that
  all commands are executed within this context. You can select other frames
  by using the `frame` command with the associated number from the stack trace.

  To find out why the test failed, print out some variables to see what is
  happening. The `reply` frame contains a local variable, which should have been
  populated by the function call to `remote_api->OnStatus`:

  ```none {: .devsite-terminal data-terminal-prefix="[zxdb]" }
  print reply
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

  From the output, you can see the `reply` variable has been filled in with some
  information, the expectation is that the size of the `processes` vector should
  be equal to 3. Print the member variable of `reply` to see more information.
  You can also print the size method of that vector (general function calling
  support is not implemented yet):

  ```none {: .devsite-terminal data-terminal-prefix="[zxdb]" }
  print reply.processes
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

  It seems that the test's expectations are slightly incorrect. You only
  injected 2 mock processes, but the test was expecting 3. You can simply
  update the test to expect the size of the `reply.processes` vector to be be
  2 instead of 3. You can now close zxdb with `quit` to then update and fix the
  tests:

  ```none {: .devsite-terminal data-terminal-prefix="[zxdb]" }
  quit

  <...fx test output continues...>

  Failed tests: DebugAgentTests.OnGlobalStatus <-- Failed test case that we debugged.
  175 out of 176 attempted tests passed, 2 tests skipped...
  fuchsia-pkg://fuchsia.com/debug_agent_unit_tests?hash=3f6d97801bb147034a344e3fe1bb69291a7b690b9d3d075246ddcba59397ac12#meta/debug_agent_unit_tests.cm completed with result: FAILED
  Tests failed.


  FAILED: fuchsia-pkg://fuchsia.com/debug_agent_unit_tests#meta/debug_agent_unit_tests.cm
  ```

  Now that you have found the source of the test failure, you can fix the test:

  ```diff
  -ASSERT_EQ(reply.processes.size(), 3u)
  +ASSERT_EQ(reply.processes.size(), 2u)
  ```

  Then, run `fx test`:

  ```posix-terminal
  fx test --break-on-failure debug_agent_unit_tests
  ```

  The output should look like:

  ```none {:.devsite-disable-click-to-copy}
  You are using the new fx test, which is currently ready for general use ✅
  See details here: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/scripts/fxtest/rewrite
  To go back to the old fx test, use `fx --enable=legacy_fxtest test`, and please file a bug under b/293917801.

  Default flags loaded from /usr/local/google/home/jruthe/.fxtestrc:
  []

  Logging all output to: /usr/local/google/home/jruthe/upstream/fuchsia/out/workbench_eng.x64/fxtest-2024-03-25T15:56:31.874893.log.json.gz
  Use the `--logpath` argument to specify a log location or `--no-log` to disable

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

  zxdb no longer appears, because you have successfully fixed all of the test
  failures!

[archivist-test-example]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/diagnostics/archivist/src/archivist.rs;l=539-549;drc=b266522960501274fbe62ac9a0d0f9631a2a58b6
[debug-agent-test-example]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/developer/debug/debug_agent/debug_agent_unittest.cc;l=63-147
[rust-test-runner]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/sys/test_runners/rust/
[rust-test-runner-parallel-default]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/sys/test_runners/rust/src/test_server.rs;l=55
[zxdb-test-doc]: /docs/development/debugger/tests.md
[zxdb-parallel-tests]: /docs/development/debugger/tests.md#executing-test-cases-in-parallel
