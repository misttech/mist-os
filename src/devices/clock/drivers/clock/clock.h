// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_CLOCK_DRIVERS_CLOCK_CLOCK_H_
#define SRC_DEVICES_CLOCK_DRIVERS_CLOCK_CLOCK_H_

#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <fidl/fuchsia.hardware.clockimpl/cpp/driver/wire.h>
#include <fuchsia/hardware/clockimpl/cpp/banjo.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/platform-defs.h>

#include <ddktl/device.h>

class ClockImplProxy {
 public:
  static zx::result<ClockImplProxy> Create(zx_device_t* parent);

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

class ClockDevice;
using ClockDeviceType = ddk::Device<ClockDevice>;

class ClockDevice : public ClockDeviceType, public fidl::WireServer<fuchsia_hardware_clock::Clock> {
 public:
  ClockDevice(zx_device_t* parent, ClockImplProxy clock, uint32_t id)
      : ClockDeviceType(parent), clock_(std::move(clock)), id_(id) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  // Device protocol implementation
  void DdkRelease();

  zx_status_t ServeOutgoing(fidl::ServerEnd<fuchsia_io::Directory> server_end);

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

  const ClockImplProxy clock_;
  const uint32_t id_;

  async_dispatcher_t* dispatcher_{fdf::Dispatcher::GetCurrent()->async_dispatcher()};
  component::OutgoingDirectory outgoing_{dispatcher_};
  fidl::ServerBindingGroup<fuchsia_hardware_clock::Clock> bindings_;
};

class ClockInitDevice;
using ClockInitDeviceType = ddk::Device<ClockInitDevice>;

class ClockInitDevice : public ClockInitDeviceType {
 public:
  static void Create(zx_device_t* parent, const ClockImplProxy& clock);

  explicit ClockInitDevice(zx_device_t* parent) : ClockInitDeviceType(parent) {}

  void DdkRelease() { delete this; }

 private:
  static zx_status_t ConfigureClocks(const fuchsia_hardware_clockimpl::wire::InitMetadata& metadata,
                                     const ClockImplProxy& clock);
};

#endif  // SRC_DEVICES_CLOCK_DRIVERS_CLOCK_CLOCK_H_
