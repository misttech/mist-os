// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_CLOCK_DRIVERS_CLOCK_CLOCK_H_
#define SRC_DEVICES_CLOCK_DRIVERS_CLOCK_CLOCK_H_

#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <fidl/fuchsia.hardware.clockimpl/cpp/driver/wire.h>
#include <fuchsia/hardware/clockimpl/cpp/banjo.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>

#include <ddk/metadata/clock.h>
#include <ddktl/device.h>

class ClockDevice : public fidl::WireServer<fuchsia_hardware_clock::Clock> {
 public:
  using AddChildCallback = fit::callback<zx_status_t(
      std::string_view, const fuchsia_driver_framework::NodePropertyVector&,
      const std::vector<fuchsia_driver_framework::Offer>&)>;

  explicit ClockDevice(uint32_t id) : id_(id) {}

  zx_status_t Init(const std::shared_ptr<fdf::Namespace>& incoming,
                   const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
                   const std::optional<std::string>& node_name,
                   AddChildCallback add_child_callback);

 private:
  // fuchsia.hardware.clock/Clock protocol implementation
  void Enable(EnableCompleter::Sync& completer) override;
  void Disable(DisableCompleter::Sync& completer) override;
  void IsEnabled(IsEnabledCompleter::Sync& completer) override;
  void SetRate(SetRateRequestView request, SetRateCompleter::Sync& completer) override;
  void QuerySupportedRate(QuerySupportedRateRequestView request,
                          QuerySupportedRateCompleter::Sync& completer) override;
  void GetRate(GetRateCompleter::Sync& completer) override;
  void SetInput(SetInputRequestView request, SetInputCompleter::Sync& completer) override;
  void GetNumInputs(GetNumInputsCompleter::Sync& completer) override;
  void GetInput(GetInputCompleter::Sync& completer) override;

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_clock::Clock> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

  fdf::Arena arena_{'CLOC'};
  fdf::WireSyncClient<fuchsia_hardware_clockimpl::ClockImpl> clock_impl_;
  const uint32_t id_;
  fidl::ClientEnd<fuchsia_driver_framework::NodeController> child_node_;
  fidl::ServerBindingGroup<fuchsia_hardware_clock::Clock> bindings_;
  compat::SyncInitializedDeviceServer compat_server_;
};

class ClockDriver : public fdf::DriverBase {
 public:
  static constexpr char kDriverName[] = "clock";

  ClockDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(dispatcher)) {}

  zx::result<> Start() override;

 private:
  static zx_status_t ConfigureClocks(
      const fuchsia_hardware_clockimpl::wire::InitMetadata& metadata,
      fdf::ClientEnd<fuchsia_hardware_clockimpl::ClockImpl> clock_impl);

  zx_status_t CreateClockDevices();

  std::vector<std::unique_ptr<ClockDevice>> clock_devices_;
  fidl::ClientEnd<fuchsia_driver_framework::NodeController> clock_init_child_node_;
};

#endif  // SRC_DEVICES_CLOCK_DRIVERS_CLOCK_CLOCK_H_
