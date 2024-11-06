// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device_server.h"

#include <fidl/fuchsia.hardware.hrtimer/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fidl/cpp/channel.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>

#include <future>
#include <limits>
#include <optional>
#include <vector>

namespace fake_hrtimer {

using fuchsia_hardware_hrtimer::DeviceGetTicksLeftResponse;
using fuchsia_hardware_hrtimer::Properties;
using fuchsia_hardware_hrtimer::Resolution;
using fuchsia_hardware_hrtimer::TimerProperties;

DeviceServer::DeviceServer() {
  zx::result topology_client_end = component::Connect<fuchsia_power_broker::Topology>();
  if (!topology_client_end.is_ok()) {
    FX_LOGS(ERROR)
        << "Synchronous error when connecting to the |"
        << fidl::DiscoverableProtocolDefaultPath<fuchsia_power_broker::Topology> << "| protocol: "
        << topology_client_end.status_string();
    return;
  }

  fidl::SyncClient topology{*std::move(topology_client_end)};

  zx::result<fidl::Endpoints<fuchsia_power_broker::Lessor>> lessor_endpoints =
      fidl::CreateEndpoints<fuchsia_power_broker::Lessor>();
  if (lessor_endpoints.is_error()) {
    FX_LOGS(ERROR) << "Couldn't create FIDL endpoints: " << lessor_endpoints.status_string();
    return;
  }

  zx::result<fidl::Endpoints<fuchsia_power_broker::ElementControl>> element_control_endpoints =
      fidl::CreateEndpoints<fuchsia_power_broker::ElementControl>();
  if (element_control_endpoints.is_error()) {
    FX_LOGS(ERROR) << "Couldn't create FIDL endpoints: "
                   << element_control_endpoints.status_string();
    return;
  }
  auto current_level_endpoints = fidl::CreateEndpoints<fuchsia_power_broker::CurrentLevel>();
  if (!current_level_endpoints.is_ok()) {
    FX_LOGS(ERROR) << "error creating CurrentLevel endpoints: "
                   << current_level_endpoints.status_string();
    return;
  }

  fuchsia_power_broker::ElementSchema schema;
  schema.element_name(std::string("fake-hrtimer"))
      .initial_current_level(fidl::ToUnderlying(fuchsia_power_broker::BinaryPowerLevel::kOff))
      .valid_levels(std::vector<uint8_t>({
          fidl::ToUnderlying(fuchsia_power_broker::BinaryPowerLevel::kOff),
          fidl::ToUnderlying(fuchsia_power_broker::BinaryPowerLevel::kOn),
      }))
      .lessor_channel(std::move(lessor_endpoints->server))
      .element_control(std::move(element_control_endpoints->server));

  fidl::Result<fuchsia_power_broker::Topology::AddElement> element =
      topology->AddElement(std::move(schema));
  if (element.is_error()) {
    FX_LOGS(ERROR) << "Failed to add element to topology: "
                   << element.error_value().FormatDescription();
    return;
  }

  element_control_client_ = std::move(element_control_endpoints->client);
  current_level_ = fidl::SyncClient(std::move(current_level_endpoints->client));
  lessor_ = fidl::SyncClient{std::move(lessor_endpoints->client)};
}

void DeviceServer::Start(StartRequest& request, StartCompleter::Sync& completer) {
  completer.Reply(zx::ok());
  std::this_thread::sleep_for(
      std::chrono::nanoseconds(request.resolution().duration().value() * (request.ticks() + 1)));
  if (event_) {
    event_->signal(0, ZX_EVENT_SIGNALED);
  }
}

void DeviceServer::Stop(StopRequest& _request, StopCompleter::Sync& completer) {
  completer.Reply(zx::ok());
}

void DeviceServer::GetTicksLeft(GetTicksLeftRequest& _request,
                                GetTicksLeftCompleter::Sync& completer) {
  completer.Reply(zx::ok(DeviceGetTicksLeftResponse().ticks(0)));
}

void DeviceServer::SetEvent(SetEventRequest& request, SetEventCompleter::Sync& completer) {
  event_.emplace(std::move(request.event()));
  completer.Reply(zx::ok());
}

void DeviceServer::StartAndWait(StartAndWaitRequest& request,
                                StartAndWaitCompleter::Sync& completer) {
  auto fut = std::async(
      std::launch::async,
      [&]() -> zx::result<fuchsia_hardware_hrtimer::DeviceStartAndWaitResponse> {
        std::this_thread::sleep_for(std::chrono::nanoseconds(
            request.resolution().duration().value() * (request.ticks() + 1)));
        if (!lessor_.has_value()) {
          FX_LOGS(ERROR) << "No active lessor";
          return zx::error(ZX_ERR_BAD_STATE);
        }
        fidl::Result<fuchsia_power_broker::Lessor::Lease> result_lease =
            lessor_.value()->Lease(fidl::ToUnderlying(fuchsia_power_broker::BinaryPowerLevel::kOn));

        if (result_lease.is_error()) {
          FX_LOGS(ERROR) << "Failed to acquire a lease: "
                         << result_lease.error_value().FormatDescription();
          return zx::error(ZX_ERR_BAD_STATE);
        }

        fuchsia_power_system::LeaseToken local_wake_token, remote_wake_token;
        zx_status_t status =
            fuchsia_power_system::LeaseToken::create(0, &local_wake_token, &remote_wake_token);
        if (status != ZX_OK) {
          FX_LOGS(ERROR) << "LeaseToken create failed: " << zx_status_get_string(status)
                         << std::endl;
        }
        fuchsia_hardware_hrtimer::DeviceStartAndWaitResponse response;
        response.keep_alive(std::move(remote_wake_token));

        if (event_) {
          event_->signal(0, ZX_EVENT_SIGNALED);
        }
        return zx::ok(std::move(response));
      });
  auto response = fut.get();
  completer.Reply(std::move(response));
}

void DeviceServer::StartAndWait2(StartAndWait2Request& request,
                                 StartAndWait2Completer::Sync& completer) {
  auto fut = std::async(
      std::launch::async,
      [&]() -> zx::result<fuchsia_hardware_hrtimer::DeviceStartAndWait2Response> {
        if (!lessor_.has_value()) {
          FX_LOGS(ERROR) << "No active lessor";
          return zx::error(ZX_ERR_BAD_STATE);
        }
        std::this_thread::sleep_for(std::chrono::nanoseconds(
            request.resolution().duration().value() * (request.ticks() + 1)));
        fidl::Result<fuchsia_power_broker::Lessor::Lease> result_lease =
            lessor_.value()->Lease(fidl::ToUnderlying(fuchsia_power_broker::BinaryPowerLevel::kOn));

        if (result_lease.is_error()) {
          FX_LOGS(ERROR) << "Failed to acquire a lease: "
                         << result_lease.error_value().FormatDescription();
          return zx::error(ZX_ERR_BAD_STATE);
        }

        zx::eventpair local_wake_lease, remote_wake_lease;
        zx_status_t status =
            fuchsia_power_system::LeaseToken::create(0, &local_wake_lease, &remote_wake_lease);
        if (status != ZX_OK) {
          FX_LOGS(ERROR) << "LeaseToken create failed: " << zx_status_get_string(status)
                         << std::endl;
        }

        fuchsia_hardware_hrtimer::DeviceStartAndWait2Response response;
        response.expiration_keep_alive(std::move(local_wake_lease));

        if (event_) {
          event_->signal(0, ZX_EVENT_SIGNALED);
        }
        return zx::ok(std::move(response));
      });
  auto response = fut.get();
  completer.Reply(std::move(response));
}

void DeviceServer::GetProperties(GetPropertiesCompleter::Sync& completer) {
  uint64_t size = 10;
  std::vector<TimerProperties> timer_properties(size);
  for (uint64_t i = 0; i < size; i++) {
    timer_properties[i] =
        TimerProperties()
            .id(i)
            .max_ticks(std::numeric_limits<uint16_t>::max())
            .supports_event(true)
            .supports_wait(true)
            .supported_resolutions(std::vector<Resolution>{{Resolution::WithDuration(1000000)}});
  }
  Properties properties = {};
  properties.timers_properties(std::move(timer_properties));
  completer.Reply(std::move(properties));
}

void DeviceServer::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_hrtimer::Device> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FX_LOGS(WARNING) << "Received an unknown method with ordinal " << metadata.method_ordinal;
}

void DeviceServer::Serve(async_dispatcher_t* dispatcher,
                         fidl::ServerEnd<fuchsia_hardware_hrtimer::Device> server) {
  bindings_.AddBinding(dispatcher, std::move(server), this, fidl::kIgnoreBindingClosure);
}

}  // namespace fake_hrtimer
