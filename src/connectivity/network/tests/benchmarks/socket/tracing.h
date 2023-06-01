// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_TESTS_BENCHMARKS_SOCKET_TRACING_H_
#define SRC_CONNECTIVITY_NETWORK_TESTS_BENCHMARKS_SOCKET_TRACING_H_

#include <fidl/fuchsia.tracing.controller/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <future>

inline constexpr const char* kSocketBenchmarksTracingCategory = "socket_benchmarks";

struct Tracer {
  fidl::SyncClient<fuchsia_tracing_controller::Controller> controller;
  std::future<zx_status_t> future;
};

fit::result<fit::failed, Tracer> StartTracing();
fit::result<fit::failed> StopTracing(Tracer tracer);

#endif  // SRC_CONNECTIVITY_NETWORK_TESTS_BENCHMARKS_SOCKET_TRACING_H_
