# Use the debugger to automatically debug tests

`zxdb` may be used in conjunction with [`fx test`][fx-test] using the
`--break-on-failure` or `--breakpoint` flags.

If your test uses a compatible test runner (gTest, gUnit, or Rust today),
then test failures will pause test execution for that suite and show you the
debugger prompt, for example:

```none {:.devsite-disable-click-to-copy}
=>  fx test --break-on-failure rust_crasher_test.cm

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

From this point, you may use `zxdb` as normal. Since the thread is already in a
fatal exception, typical execution commands (for example `step`, `next`, and
`until`) will not be available.

Inspection commands such as `print`, `frame`, and `backtrace` will be available
for the duration of the debugging session.

## Test failures in parallel

Sometimes multiple test cases may fail in parallel, depending on the options
given to `fx test` or test runner default configurations. Supported test runners
all spawn an individual process for each test case, and the default
configuration may allow for multiple test processes to be running at the same
time.

When running with `fx test`, `zxdb` will attach to _all_ processes in your
test's realm, and any test case failures will stop _only_ that particular
process. Parallel execution will only stop if and only if the number of test
case failures is equal to the number of allowed parallel test cases. When any
processes is detached from `zxdb`, another test case process will begin
immediately.

`zxdb` is designed to handle multi-process debugging well. You can inspect the
currently attached processes and their current execution states with the
`process` noun, or simply with the `status` command. The currently "active"
process will be indicated with a "▶". Find more detailed information about the
[interaction model][interaction-model] or via the `help` command.

## Closing the debugger

When you're finished inspecting your test, you can continue by detaching from
your test in any way (for example, `kill`, `detach`, and `continue`).

As discussed [above][#test-failures-in-parallel], multiple test cases may fail
in parallel. If you do not explicitly detach from all attached processes, `zxdb`
will remain in the foreground. You can see all attached processes using the
`process` noun.

Certain commands will detach from everything (for example, `quit`, `detach *`,
and `ctrl+d`) and resume execution of the test suite immediately.

## C++ gTest Example

Take some sample [C++ test code][debug-agent-test-example] using gTest
(abbreviated for brevity):

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

We execute the tests with the `fx test --break-on-failure` command, for example:

```none {:.devsite-disable-click-to-copy}
=>  fx test --break-on-failure debug_agent_unit_tests

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

We caught a test failure, gTest has an option to insert a software breakpoint in
the path of a test failure, which is inlined into our test. We can view the code
of the current frame with `list`, for example:

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

But that's not the code we're interested in. When we look at a stack trace, we
see code from our test is in frame #2:

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

We can view our test's source code by using the `frame` noun with an index as a
prefix to our `list` command, for example:

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

That's inconvenient, we have to type `frame 2` before every command to interact
with the part of our code we're interested in. Notice the "▶" from the output of
`frame`. It points to frame 0, indicating that it is the "active" frame.
Let's select our frame as the "active" frame with just the `frame` noun with the
frame's index from above so we can work directly with what we want to look at:

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

Now, all commands we run will be in the context of frame #2. Let's list the
source code again to be sure:

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

Cool! Now, why did the test fail? Let's print out some variables to see what's
going on. We have a local variable in this frame, `reply`, which should have
been populated by the function call to `remote_api->OnStatus`:

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

Okay, so the `reply` variable has been filled in with some information, the
expectation is that the size of the `processes` vector should be equal to 3.
Let's just print that member variable of `reply` to get a clearer picture. We
can also print the size method of that vector (general function calling
support is not implemented yet):

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

Aha, so the test expectation is wrong, we only injected 2 mock processes in our
test, but expected there to be 3. The test simply needs to be updated to expect
the size of the `reply.processes` vector to be 2 instead of 3. We can close the
debugger now to finish up the tests and then fix our test:

```none {:.devsite-disable-click-to-copy}
[zxdb] detach *

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
=>  fx test --break-on-failure debug_agent_unit_tests

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
=>
```

The debugger no longer appears, because we don't have any other test failures!
Woohoo \o/

<!-- Rust Example TODO(b/331649115) -->

<!-- Reference links -->

[debug-agent-test-example]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/developer/debug/debug_agent/debug_agent_unittest.cc;l=63-147
[fx-test]: /docs/reference/testing/fx-test.md
[interaction-model]: /docs/development/debugger/commands.md
