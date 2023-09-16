// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/lib/perfmon/controller.h"

#include <fidl/fuchsia.perfmon.cpu/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>
#include <lib/syslog/cpp/macros.h>
#include <sys/stat.h>

#include <memory>

#include <fbl/algorithm.h>

#include "src/lib/fxl/strings/string_printf.h"
#include "src/performance/lib/perfmon/controller_impl.h"
#include "src/performance/lib/perfmon/properties_impl.h"

namespace perfmon {

const char kPerfMonDev[] = "/dev/sys/platform/00:00:1d/perfmon";

static uint32_t RoundUpToPages(uint32_t value) {
  uint32_t size = fbl::round_up(value, Controller::kPageSize);
  FX_DCHECK(size & ~(Controller::kPageSize - 1));
  return size >> Controller::kLog2PageSize;
}

static uint32_t GetBufferSizeInPages(CollectionMode mode, uint32_t requested_size_in_pages) {
  switch (mode) {
    case CollectionMode::kSample:
      return requested_size_in_pages;
    case CollectionMode::kTally: {
      // For tally mode we just need something large enough to hold
      // the header + records for each event.
      unsigned num_events = kMaxNumEvents;
      uint32_t size = (sizeof(BufferHeader) + num_events * sizeof(ValueRecord));
      return RoundUpToPages(size);
    }
    default:
      __UNREACHABLE;
  }
}

bool Controller::IsSupported() {
  // The device path isn't present if it's not supported.
  struct stat stat_buffer;
  if (stat(kPerfMonDev, &stat_buffer) != 0)
    return false;
  return S_ISCHR(stat_buffer.st_mode);
}

bool Controller::GetProperties(Properties* props) {
  zx::result<fidl::ClientEnd<fuchsia_perfmon_cpu::Device>> client_end =
      component::Connect<fuchsia_perfmon_cpu::Device>(kPerfMonDev);
  if (client_end.is_error()) {
    FX_PLOGS(ERROR, client_end.error_value()) << "Error connecting to " << kPerfMonDev;
    return false;
  }
  fidl::SyncClient<fuchsia_perfmon_cpu::Device> device_client{std::move(*client_end)};

  auto controller_endpoints = fidl::CreateEndpoints<fuchsia_perfmon_cpu::Controller>();
  if (controller_endpoints.is_error()) {
    FX_PLOGS(ERROR, controller_endpoints.error_value())
        << "Failed to create fidl endpoints for fuchsia.perfmon.cpu.Controller";
    return false;
  }
  if (auto status = device_client->OpenSession(std::move(controller_endpoints->server));
      status.is_error()) {
    FX_LOGS(ERROR) << "Error opening session on " << kPerfMonDev << ": " << status.error_value();
    return false;
  }

  fidl::SyncClient<fuchsia_perfmon_cpu::Controller> client;

  auto properties = client->GetProperties();
  if (properties.is_error()) {
    FX_LOGS(ERROR) << "Failed to get properties: " << properties.error_value();
    return false;
  }

  internal::FidlToPerfmonProperties(properties->properties(), props);
  return true;
}

static bool Initialize(const fidl::SyncClient<fuchsia_perfmon_cpu::Controller>& controller,
                       uint32_t num_traces, uint32_t buffer_size_in_pages) {
  FX_VLOGS(2) << fxl::StringPrintf("num_buffers=%u, buffer_size_in_pages=0x%x", num_traces,
                                   buffer_size_in_pages);

  auto result = controller->Initialize({{{
      .num_buffers = num_traces,
      .buffer_size_in_pages = buffer_size_in_pages,
  }}});
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Initialize failed: status=" << result.error_value();
    return false;
  }

  // TODO(dje): If we get BAD_STATE, a previous run may have crashed without
  // resetting the device. The device doesn't reset itself on close yet.
  if (result.is_error()) {
    FX_DCHECK(result.error_value().is_domain_error() &&
              result.error_value().domain_error() == ZX_ERR_BAD_STATE);
    FX_VLOGS(2) << "Got BAD_STATE trying to initialize a trace,"
                << " resetting device and trying again";
    if (auto status = controller->Stop(); status.is_error()) {
      FX_VLOGS(2) << "Stopping device failed: status=" << status.error_value();
      return false;
    }
    if (auto status = controller->Terminate(); status.is_error()) {
      FX_VLOGS(2) << "Terminating previous trace failed: status=" << status.error_value();
      return false;
    }
    auto status = controller->Initialize(
        {{{.num_buffers = num_traces, .buffer_size_in_pages = buffer_size_in_pages}}});
    if (status.is_error()) {
      FX_LOGS(ERROR) << "Initialize try #2 failed: status=" << status.error_value();
      return false;
    }
    FX_VLOGS(2) << "Second Initialize attempt succeeded";
  }

  return true;
}

bool Controller::Create(uint32_t buffer_size_in_pages, const Config& config,
                        std::unique_ptr<Controller>* out_controller) {
  if (buffer_size_in_pages > kMaxBufferSizeInPages) {
    FX_LOGS(ERROR) << "Buffer size is too large, max " << kMaxBufferSizeInPages << " pages";
    return false;
  }

  zx::result<fidl::ClientEnd<fuchsia_perfmon_cpu::Device>> client_end =
      component::Connect<fuchsia_perfmon_cpu::Device>(kPerfMonDev);
  if (client_end.is_error()) {
    FX_PLOGS(ERROR, client_end.error_value()) << "Error connecting to " << kPerfMonDev;
    return false;
  }
  fidl::SyncClient<fuchsia_perfmon_cpu::Device> device_client{std::move(*client_end)};

  auto controller_endpoints = fidl::CreateEndpoints<fuchsia_perfmon_cpu::Controller>();
  if (controller_endpoints.is_error()) {
    FX_PLOGS(ERROR, controller_endpoints.error_value())
        << "Failed to create fidl endpoints for fuchsia.perfmon.cpu.Controller";
    return false;
  }
  if (auto status = device_client->OpenSession(std::move(controller_endpoints->server));
      status.is_error()) {
    FX_LOGS(ERROR) << "Error opening session on " << kPerfMonDev << ": " << status.error_value();
    return false;
  }

  fidl::SyncClient<fuchsia_perfmon_cpu::Controller> client;

  CollectionMode mode = config.GetMode();
  uint32_t num_traces = zx_system_get_num_cpus();
  // For "tally" mode we only need a small fixed amount, so toss what the
  // caller provided and use our own value.
  uint32_t actual_buffer_size_in_pages = GetBufferSizeInPages(mode, buffer_size_in_pages);

  if (!Initialize(client, num_traces, actual_buffer_size_in_pages)) {
    return false;
  }

  *out_controller = std::make_unique<internal::ControllerImpl>(std::move(client), num_traces,
                                                               buffer_size_in_pages, config);
  return true;
}

}  // namespace perfmon
