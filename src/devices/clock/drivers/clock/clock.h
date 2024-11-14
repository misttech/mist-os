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

class ClockImplProxy {
 public:
  static zx::result<ClockImplProxy> Create(const std::shared_ptr<fdf::Namespace>& incoming);

  explicit ClockImplProxy(ddk::ClockImplProtocolClient clock_banjo) : clock_banjo_(clock_banjo) {}

  explicit ClockImplProxy(fdf::ClientEnd<fuchsia_hardware_clockimpl::ClockImpl> clock_fidl)
      : clock_fidl_(std::move(clock_fidl)) {}

  ClockImplProxy(ddk::ClockImplProtocolClient clock_banjo,
                 fdf::ClientEnd<fuchsia_hardware_clockimpl::ClockImpl> clock_fidl)
      : clock_banjo_(clock_banjo), clock_fidl_(std::move(clock_fidl)) {}

  zx_status_t Enable(uint32_t id) const;
  zx_status_t Disable(uint32_t id) const;
  zx_status_t IsEnabled(uint32_t id, bool* out_enabled) const;
  zx_status_t SetRate(uint32_t id, uint64_t hz) const;
  zx_status_t QuerySupportedRate(uint32_t id, uint64_t hz, uint64_t* out_hz) const;
  zx_status_t GetRate(uint32_t id, uint64_t* out_hz) const;
  zx_status_t SetInput(uint32_t id, uint32_t idx) const;
  zx_status_t GetNumInputs(uint32_t id, uint32_t* out_n) const;
  zx_status_t GetInput(uint32_t id, uint32_t* out_index) const;

 private:
  ddk::ClockImplProtocolClient clock_banjo_;
  fdf::WireSyncClient<fuchsia_hardware_clockimpl::ClockImpl> clock_fidl_;
};

class ClockDevice : public fidl::WireServer<fuchsia_hardware_clock::Clock> {
 public:
  using AddChildCallback = fit::callback<zx_status_t(
      std::string_view, const fuchsia_driver_framework::NodePropertyVector&,
      const std::vector<fuchsia_driver_framework::Offer>&)>;

  ClockDevice(uint32_t id, ClockImplProxy clock_impl) : clock_(std::move(clock_impl)), id_(id) {}

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

  ClockImplProxy clock_;
  const uint32_t id_;
  std::optional<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> child_node_;
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
  zx_status_t ConfigureClocks(const fuchsia_hardware_clockimpl::wire::InitMetadata& metadata,
                              const ClockImplProxy& clock);

  zx_status_t CreateClockDevices();

  std::vector<std::unique_ptr<ClockDevice>> clock_devices_;
  fidl::ClientEnd<fuchsia_driver_framework::NodeController> clock_init_child_node_;
};

#endif  // SRC_DEVICES_CLOCK_DRIVERS_CLOCK_CLOCK_H_
