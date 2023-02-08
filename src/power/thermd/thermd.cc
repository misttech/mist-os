// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cpuid.h>
#include <errno.h>
#include <fcntl.h>
#include <fidl/fuchsia.hardware.thermal/cpp/wire.h>
#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <inttypes.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/watcher.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/provider.h>
#include <lib/trace/event.h>
#include <lib/zx/channel.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/system.h>

#include <future>

#include <fbl/unique_fd.h>

// degrees Celsius below threshold before we adjust PL value
constexpr float kCoolThresholdCelsius = 5.0f;

class PlatformConfiguration {
 public:
  static std::unique_ptr<PlatformConfiguration> Create(zx::resource);

  zx_status_t SetMinPL1() { return SetPL1Mw(pl1_min_mw_); }
  zx_status_t SetMaxPL1() { return SetPL1Mw(pl1_max_mw_); }

  bool IsAtMax() { return current_pl1_mw_ == pl1_max_mw_; }
  bool IsAtMin() { return current_pl1_mw_ == pl1_min_mw_; }

 private:
  PlatformConfiguration(uint32_t pl1_min_mw, uint32_t pl1_max_mw, zx::resource power_resource)
      : pl1_min_mw_(pl1_min_mw),
        pl1_max_mw_(pl1_max_mw),
        power_resource_(std::move(power_resource)) {}

  zx_status_t SetPL1Mw(uint32_t target_mw);

  const uint32_t pl1_min_mw_;
  const uint32_t pl1_max_mw_;
  const zx::resource power_resource_;

  static constexpr uint32_t kEvePL1MinMw = 2500;
  static constexpr uint32_t kEvePL1MaxMw = 7000;

  static constexpr uint32_t kAtlasPL1MinMw = 3000;
  static constexpr uint32_t kAtlasPL1MaxMw = 7000;

  uint32_t current_pl1_mw_;
};

zx::result<zx::resource> get_power_resource() {
  zx::result client_end = component::Connect<fuchsia_kernel::PowerResource>();
  if (client_end.is_error()) {
    FX_PLOGS(ERROR, client_end.status_value()) << "Failed to open fuchsia.kernel.PowerResource";
    return client_end.take_error();
  }
  fidl::WireSyncClient client{std::move(client_end.value())};
  fidl::WireResult result = client->Get();
  if (!result.ok()) {
    FX_PLOGS(ERROR, result.status()) << "FIDL error while trying to get power resource";
    return zx::error(result.status());
  }
  return zx::ok(std::move(result.value().resource));
}

std::unique_ptr<PlatformConfiguration> PlatformConfiguration::Create(zx::resource power_resource) {
  unsigned int a, b, c, d;
  unsigned int leaf_num = 0x80000002;
  char brand_string[50];
  memset(brand_string, 0, sizeof(brand_string));
  for (int i = 0; i < 3; i++) {
    if (!__get_cpuid(leaf_num + i, &a, &b, &c, &d)) {
      return nullptr;
    }
    memcpy(brand_string + (i * 16), &a, sizeof(uint32_t));
    memcpy(brand_string + (i * 16) + 4, &b, sizeof(uint32_t));
    memcpy(brand_string + (i * 16) + 8, &c, sizeof(uint32_t));
    memcpy(brand_string + (i * 16) + 12, &d, sizeof(uint32_t));
  }
  // Only run thermd for processors used in Pixelbooks. The PL1 min/max settings are specified by
  // the chipset.
  if (strstr(brand_string, "i5-7Y57") || strstr(brand_string, "i7-7Y75")) {
    return std::unique_ptr<PlatformConfiguration>(
        new PlatformConfiguration(kEvePL1MinMw, kEvePL1MaxMw, std::move(power_resource)));
  } else if (strstr(brand_string, "i5-8200Y") || strstr(brand_string, "i7-8500Y") ||
             strstr(brand_string, "m3-8100Y")) {
    return std::unique_ptr<PlatformConfiguration>(
        new PlatformConfiguration(kAtlasPL1MinMw, kAtlasPL1MaxMw, std::move(power_resource)));
  }
  return nullptr;
}

zx_status_t PlatformConfiguration::SetPL1Mw(uint32_t target_mw) {
  zx_system_powerctl_arg_t arg = {
      .x86_power_limit =
          {
              .power_limit = target_mw,
              .time_window = 0,
              .clamp = 1,
              .enable = 1,
          },
  };
  zx_status_t status =
      zx_system_powerctl(power_resource_.get(), ZX_SYSTEM_POWERCTL_X86_SET_PKG_PL1, &arg);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to set PL1 to " << target_mw;
    return status;
  }
  current_pl1_mw_ = target_mw;
  TRACE_COUNTER("thermal", "throttle", 0, "pl1", target_mw);
  return ZX_OK;
}

void start_trace() {
  // Create a message loop
  static async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  static trace::TraceProviderWithFdio trace_provider(loop.dispatcher());
  static bool started = false;
  if (!started) {
    loop.StartThread();
    started = true;
  }
}

// TODO(fxbug.dev/108619): This code here needs an update, it's using some very old patterns.
zx_status_t RunThermd() {
  zx::result power_resource = get_power_resource();
  if (power_resource.is_error()) {
    FX_PLOGS(ERROR, power_resource.status_value()) << "Failed to get power resource";
    return power_resource.status_value();
  }

  auto config = PlatformConfiguration::Create(std::move(power_resource.value()));
  if (!config) {
    // If there is no platform configuration then we should warn since thermd should only be
    // included on devices where we expect it to run.
    FX_LOGS(WARNING) << "no platform configuration found";
    return ZX_ERR_NOT_FOUND;
  }

  FX_LOGS(INFO) << "started";

  start_trace();

  zx_nanosleep(zx_deadline_after(ZX_SEC(3)));

  fbl::unique_fd dirfd;
  if (dirfd.reset(open("/dev/class/thermal", O_DIRECTORY | O_RDONLY)) < 0) {
    FX_LOGS(ERROR) << "Failed to open /dev/class/thermal: " << strerror(errno);
    return ZX_ERR_IO;
  }

  std::optional<zx::result<fidl::ClientEnd<fuchsia_hardware_thermal::Device>>> device;
  if (zx_status_t status = fdio_watch_directory(
          dirfd.get(),
          [](int dirfd, int event, const char* name, void* cookie) {
            if (event != WATCH_EVENT_ADD_FILE) {
              return ZX_OK;
            }
            if (std::string_view{name} == ".") {
              return ZX_OK;
            }
            // first sensor is ambient sensor
            // TODO(fxbug.dev/108619): come up with a way to detect this is the ambient sensor
            auto& device = *static_cast<
                std::optional<zx::result<fidl::ClientEnd<fuchsia_hardware_thermal::Device>>>*>(
                cookie);
            fdio_cpp::UnownedFdioCaller caller(dirfd);
            device.emplace(
                component::ConnectAt<fuchsia_hardware_thermal::Device>(caller.directory(), name));
            return ZX_ERR_STOP;
          },
          ZX_TIME_INFINITE, &device);
      status != ZX_ERR_STOP) {
    FX_PLOGS(ERROR, status) << "watcher terminating without finding sensors, terminating thermd...";
    return status;
  }

  if (!device.has_value()) {
    FX_LOGS(ERROR) << "watcher did not find sensors, terminating thermd...";
    return ZX_ERR_NOT_FOUND;
  }
  if (device.value().is_error()) {
    FX_PLOGS(ERROR, device.value().status_value()) << "failed to connect to sensor";
    return device.value().status_value();
  }
  fidl::WireSyncClient client{std::move(device.value().value())};

  float temp;
  {
    const fidl::WireResult result = client->GetTemperatureCelsius();
    if (!result.ok()) {
      FX_PLOGS(ERROR, result.status()) << "Failed to get temperature";
      return result.status();
    }
    const fidl::WireResponse response = result.value();
    if (zx_status_t status = response.status; status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to get temperature";
      return status;
    }
    temp = response.temp;
    TRACE_COUNTER("thermal", "temp", 0, "ambient-c", temp);
  }
  fuchsia_hardware_thermal::wire::ThermalInfo info;
  {
    const fidl::WireResult result = client->GetInfo();
    if (!result.ok()) {
      FX_PLOGS(ERROR, result.status()) << "Failed to get info";
      return result.status();
    }
    const fidl::WireResponse response = result.value();
    if (zx_status_t status = response.status; status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to get info";
      return status;
    }
    info = *response.info;
    TRACE_COUNTER("thermal", "trip-point", 0, "passive-c", info.passive_temp_celsius, "critical-c",
                  info.critical_temp_celsius);
    if (info.max_trip_count == 0) {
      FX_LOGS(ERROR) << "Trip points not supported, exiting";
      return ZX_ERR_NOT_SUPPORTED;
    }
  }
  zx::event h;
  {
    fidl::WireResult result = client->GetStateChangeEvent();
    if (!result.ok()) {
      FX_PLOGS(ERROR, result.status()) << "Failed to get state change event";
      return result.status();
    }
    auto& response = result.value();
    if (zx_status_t status = response.status; status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to get state change event";
      return status;
    }
    h = std::move(response.handle);
  }
  // Set a trip point.
  {
    const fidl::WireResult result = client->SetTripCelsius(0, info.passive_temp_celsius);
    if (!result.ok()) {
      FX_PLOGS(ERROR, result.status()) << "Failed to set trip point";
      return result.status();
    }
    const auto& response = result.value();
    if (zx_status_t status = response.status; status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to set trip point";
      return status;
    }
  }
  // Update info.
  {
    const fidl::WireResult result = client->GetInfo();
    if (!result.ok()) {
      FX_PLOGS(ERROR, result.status()) << "Failed to get info";
      return result.status();
    }
    const fidl::WireResponse response = result.value();
    if (zx_status_t status = response.status; status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to get info";
      return status;
    }
    info = *response.info;
    TRACE_COUNTER("thermal", "trip-point", 0, "passive-c", info.passive_temp_celsius, "critical-c",
                  info.critical_temp_celsius, "active0-c", info.active_trip[0]);
  }

  // Set PL1 to the platform maximum.
  config->SetMaxPL1();

  for (;;) {
    zx_signals_t observed = 0;
    zx_status_t status = h.wait_one(ZX_USER_SIGNAL_0, zx::deadline_after(zx::sec(1)), &observed);
    if ((status != ZX_OK) && (status != ZX_ERR_TIMED_OUT)) {
      FX_PLOGS(ERROR, status) << "Failed to wait on event";
      return status;
    }
    if (observed & ZX_USER_SIGNAL_0) {
      const fidl::WireResult result = client->GetInfo();
      if (!result.ok()) {
        FX_PLOGS(ERROR, result.status()) << "Failed to get info";
        return result.status();
      }
      const fidl::WireResponse response = result.value();
      if (zx_status_t status = response.status; status != ZX_OK) {
        FX_PLOGS(ERROR, status) << "Failed to get info";
        return status;
      }
      info = *response.info;
      if (info.state) {
        // Decrease power limit
        config->SetMinPL1();

        const fidl::WireResult result = client->GetTemperatureCelsius();
        if (!result.ok()) {
          FX_PLOGS(ERROR, result.status()) << "Failed to get temperature";
          return result.status();
        }
        const fidl::WireResponse response = result.value();
        if (zx_status_t status = response.status; status != ZX_OK) {
          FX_PLOGS(ERROR, status) << "Failed to get temperature";
          return status;
        }
        temp = response.temp;
      } else {
        TRACE_COUNTER("thermal", "event", 0, "spurious", temp);
      }
    }
    if (status == ZX_ERR_TIMED_OUT) {
      const fidl::WireResult result = client->GetTemperatureCelsius();
      if (!result.ok()) {
        FX_PLOGS(ERROR, result.status()) << "Failed to get temperature";
        return result.status();
      }
      const fidl::WireResponse response = result.value();
      if (zx_status_t status = response.status; status != ZX_OK) {
        FX_PLOGS(ERROR, status) << "Failed to get temperature";
        return status;
      }
      temp = response.temp;
      TRACE_COUNTER("thermal", "temp", 0, "ambient-c", temp);

      // Increase power limit if the temperature dropped enough
      if (temp < info.active_trip[0] - kCoolThresholdCelsius && !config->IsAtMax()) {
        // Make sure the state is clear
        const fidl::WireResult result = client->GetInfo();
        if (!result.ok()) {
          FX_PLOGS(ERROR, result.status()) << "Failed to get info";
          return result.status();
        }
        const fidl::WireResponse response = result.value();
        if (zx_status_t status = response.status; status != ZX_OK) {
          FX_PLOGS(ERROR, status) << "Failed to get info";
          return status;
        }
        info = *response.info;
        if (!info.state) {
          config->SetMaxPL1();
        }
      }

      if (temp > info.active_trip[0] && !config->IsAtMin()) {
        // Decrease power limit
        config->SetMinPL1();
      }
    }
  }
  // Do not return so that the compiler will catch it if this becomes reachable.
}

int main(int argc, char** argv) {
  zx_status_t status = RunThermd();

  // RunThermd never returns successfully, so always treat this as an error path.
  FX_LOGS(ERROR) << "Exited with status: " << zx_status_get_string(status);

  // TODO(https://fxbug.dev/97657): Hang around. If we exit before archivist has started, our logs
  // will be lost, and it's important that we know that thermd is failing and why.
  std::promise<void>().get_future().wait();

  return -1;
}
