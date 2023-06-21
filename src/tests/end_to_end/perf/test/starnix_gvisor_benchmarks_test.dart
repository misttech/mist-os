// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'package:test/test.dart';
import 'helpers.dart';

// This runs prebuilt gVisor benchmarks on Starnix.
// The benchmarks come from here:
// https://github.com/google/gvisor/tree/master/test/perf/linux

List<void Function()> _tests = [];

void main(List<String> args) {
  const starnix_gvisor_benchmarks = 'starnix_gvisor_benchmarks';

  enableLoggingOutput();

  const benchmarks = {
    'clock_getres': '--benchmark_filter_internal=all',
    'clock_gettime': '--benchmark_filter_internal=all',
    // 'death': '--benchmark_filter_internal=all', not passing
    'epoll': '--benchmark_filter_internal=all',
    'getdents': '--benchmark_filter_internal=all',
    'getpid': '--benchmark_filter_internal=all',
    'open': '--benchmark_filter_internal=all',
    'open_read_close': '--benchmark_filter_internal=all',
    'signal': '--benchmark_filter_internal=all',
    'stat': '--benchmark_filter_internal=all',
    // TODO(b/275745984): Update to all when BM_ThreadSwitch passes.
    'fork': '--benchmark_filter_internal="'
        'BM_CPUBoundSymmetric'
        '|BM_CPUBoundUniprocess'
        '|BM_CPUBoundAsymmetric'
        '|BM_ProcessSwitch'
        '|BM_ThreadStart'
        '|BM_ProcessLifecycle"',
    // TODO(b/275745984): Add BM_RecvmsgWithControlBuf and
    // variants of BM_SendmsgTCP when they pass.
    'send_recv': '--benchmark_filter_internal="'
        'BM_Recvmsg/real_time'
        '|BM_Sendmsg/real_time'
        '|BM_Recvfrom/real_time'
        '|BM_Sendto/real_time"',
    // The following benchmarks run with large ranges of arguments.
    // Run a subset of them.
    'dup': '--benchmark_filter_internal="'
        'BM_Dup/1/real_time'
        '|BM_Dup/8/real_time'
        '|BM_Dup/64/real_time'
        '|BM_Dup/512/real_time'
        '|BM_Dup/4096/real_time"',
    'futex': '--benchmark_filter_internal="'
        'BM_FutexRoundtripDelayed/0/min_time'
        '|BM_FutexRoundtripDelayed/10/min_time'
        '|BM_FutexWaitMonotonicDeadline'
        '|BM_FutexWaitMonotonicTimeout/1/min_time'
        '|BM_FutexWakeNop'
        '|BM_FutexWaitNop'
        '|BM_FutexWaitRealtimeDeadline"',
    'gettid': '--benchmark_filter_internal="'
        r'(BM_Gettid/real_time/threads:1'
        r'|BM_Gettid/real_time/threads:2'
        r'|BM_Gettid/real_time/threads:4'
        r'|BM_Gettid/real_time/threads:8'
        r'|BM_Gettid/real_time/threads:16)$"',
    'mapping': '--benchmark_filter_internal="'
        'BM_MapUnmap/1/real_time'
        '|BM_MapUnmap/8/real_time'
        '|BM_MapUnmap/64/real_time'
        '|BM_MapUnmap/512/real_time'
        '|BM_MapUnmap/4096/real_time'
        '|BM_MapTouchUnmap/1/real_time'
        '|BM_MapTouchUnmap/8/real_time'
        '|BM_MapTouchUnmap/64/real_time'
        '|BM_MapTouchUnmap/512/real_time'
        '|BM_MapTouchUnmap/4096/real_time'
        '|BM_MapTouchMany/1/real_time'
        '|BM_MapTouchMany/8/real_time'
        '|BM_MapTouchMany/64/real_time'
        '|BM_MapTouchMany/512/real_time'
        '|BM_MapTouchMany/4096/real_time'
        '|BM_PageFault/real_time"',
    'pipe': '--benchmark_filter_internal="'
        'BM_Pipe/8/real_time'
        '|BM_Pipe/1/real_time'
        '|BM_Pipe/64/real_time'
        '|BM_Pipe/512/real_time'
        '|BM_Pipe/4096/real_time"',
    'randread': '--benchmark_filter_internal="'
        'BM_RandRead/1/real_time'
        '|BM_RandRead/8/real_time'
        '|BM_RandRead/64/real_time'
        '|BM_RandRead/512/real_time'
        '|BM_RandRead/4096/real_time"',
    'read': '--benchmark_filter_internal="'
        'BM_Read/1/real_time'
        '|BM_Read/8/real_time'
        '|BM_Read/64/real_time'
        '|BM_Read/512/real_time'
        '|BM_Read/4096/real_time"',
    'sched_yield': '--benchmark_filter_internal="'
        r'(BM_Sched_yield/real_time/threads:1'
        r'|BM_Sched_yield/real_time/threads:2'
        r'|BM_Sched_yield/real_time/threads:4'
        r'|BM_Sched_yield/real_time/threads:8'
        r'|BM_Sched_yield/real_time/threads:16)$"',
    'seqwrite': '--benchmark_filter_internal="'
        'BM_SeqWrite/1/real_time'
        '|BM_SeqWrite/8/real_time'
        '|BM_SeqWrite/64/real_time'
        '|BM_SeqWrite/512/real_time'
        '|BM_SeqWrite/4096/real_time"',
    'sleep': '--benchmark_filter_internal="'
        'BM_Sleep/0/real_time'
        '|BM_Sleep/1/real_time'
        '|BM_Sleep/1000/real_time'
        '|BM_Sleep/1000000/real_time'
        '|BM_Sleep/10000000/real_time"',
    'unlink': '--benchmark_filter_internal="'
        'BM_Unlink/1/real_time'
        '|BM_Unlink/8/real_time'
        '|BM_Unlink/64/real_time'
        '|BM_Unlink/512/real_time'
        '|BM_Unlink/4096/real_time"',
    'write': '--benchmark_filter_internal="'
        'BM_Write/1/real_time'
        '|BM_Write/8/real_time'
        '|BM_Write/64/real_time'
        '|BM_Write/512/real_time'
        '|BM_Write/4096/real_time'
        '|BM_Append"',
  };

  benchmarks.forEach((String componentName, String commandArgs) {
    _tests.add(() {
      test(starnix_gvisor_benchmarks, () async {
        await runTestComponent(
            packageName: starnix_gvisor_benchmarks,
            componentName: '$componentName\_benchmark.cm',
            realm: "/core/testing:starnix-tests",
            commandArgs: commandArgs,
            expectedMetricNamesFile:
                'fuchsia.starnix.gvisor_benchmarks.$componentName.txt');
      }, timeout: Timeout.none);
    });
  });

  runShardTests(args, _tests);
}
