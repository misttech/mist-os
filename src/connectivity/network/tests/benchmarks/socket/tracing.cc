// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tracing.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/start.h>

#include <fstream>

fit::result<fit::failed, Tracer> StartTracing() {
  trace_provider_start();

  zx::socket trace_socket, outgoing_socket;
  if (zx_status_t status = zx::socket::create(ZX_SOCKET_STREAM, &trace_socket, &outgoing_socket);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to create zircon socket";
    return fit::failed();
  }

  fidl::SyncClient<fuchsia_tracing_controller::Controller> controller;
  {
    zx::result client_end = component::Connect<fuchsia_tracing_controller::Controller>();
    if (client_end.is_error()) {
      FX_PLOGS(ERROR, client_end.status_value())
          << "failed to connect to fuchsia.tracing.controller/Controller";
      return fit::failed();
    }
    controller.Bind(std::move(*client_end));
  }

  const fuchsia_tracing_controller::TraceConfig trace_config{{
      .categories = std::vector<std::string>{"kernel:sched", "kernel:meta", "net", "perftest",
                                             kSocketBenchmarksTracingCategory},
      .buffer_size_megabytes_hint = 64,
  }};
  if (fit::result<fidl::OneWayError> response =
          controller->InitializeTracing({trace_config, std::move(outgoing_socket)});
      response.is_error()) {
    FX_LOGS(ERROR) << "failed to initialize tracing: " << response.error_value();
    return fit::failed();
  }

  if (fidl::Result<fuchsia_tracing_controller::Controller::StartTracing> response =
          controller->StartTracing({});
      response.is_error()) {
    FX_LOGS(ERROR) << "failed to start tracing: " << response.error_value();
    return fit::failed();
  }

  std::future<zx_status_t> fut = std::async(
      std::launch::async,
      [](zx::socket trace_socket) {
        std::ofstream ofs("/custom_artifacts/trace.fxt");
        std::array<char, 4096> buffer;
        for (;;) {
          zx_signals_t signals = 0;
          zx_status_t status = trace_socket.wait_one(ZX_SOCKET_READABLE | ZX_SOCKET_PEER_CLOSED,
                                                     zx::time::infinite(), &signals);
          if (status != ZX_OK) {
            return status;
          }
          if (signals & ZX_SOCKET_READABLE) {
            size_t actual;
            zx_status_t status =
                trace_socket.read(0u /* options */, buffer.data(), buffer.size(), &actual);
            switch (status) {
              case ZX_ERR_PEER_CLOSED:
                return ZX_OK;
              case ZX_OK:
                ofs.write(buffer.data(), actual);
                break;
              default:
                return status;
            }
          }
          if (signals & ZX_SOCKET_PEER_CLOSED) {
            return ZX_OK;
          }
        }
      },
      std::move(trace_socket));
  return fit::ok(Tracer{.controller = std::move(controller), .future = std::move(fut)});
}

fit::result<fit::failed> StopTracing(Tracer tracer) {
  fuchsia_tracing_controller::TerminateOptions terminate_options;
  terminate_options.write_results(true);

  if (fidl::Result<fuchsia_tracing_controller::Controller::TerminateTracing> response =
          tracer.controller->TerminateTracing({terminate_options});
      response.is_error()) {
    FX_LOGS(ERROR) << "failed to terminate tracing: " << response.error_value();
    return fit::failed();
  }

  tracer.future.wait();
  if (zx_status_t status = tracer.future.get(); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "error reading trace";
    return fit::failed();
  }
  return fit::ok();
}
