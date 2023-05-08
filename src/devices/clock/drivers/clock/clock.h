// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_CLOCK_DRIVERS_CLOCK_CLOCK_H_
#define SRC_DEVICES_CLOCK_DRIVERS_CLOCK_CLOCK_H_

#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <fuchsia/hardware/clock/cpp/banjo.h>
#include <fuchsia/hardware/clockimpl/cpp/banjo.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/platform-defs.h>

#include <ddktl/device.h>

class ClockDevice;
using ClockDeviceType = ddk::Device<ClockDevice>;

class ClockDevice : public ClockDeviceType,
                    // TODO(fxbug.dev/126069): Remove banjo clock protocol when
                    // there aren't any banjo clock protocol clients.
                    public ddk::ClockProtocol<ClockDevice, ddk::base_protocol>,
                    public fidl::WireServer<fuchsia_hardware_clock::Clock> {
 public:
  ClockDevice(zx_device_t* parent, clock_impl_protocol_t* clock, uint32_t id)
      : ClockDeviceType(parent), clock_(clock), id_(id) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  // Device protocol implementation
  void DdkRelease();

  zx_status_t ClockEnable();
  zx_status_t ClockDisable();
  zx_status_t ClockIsEnabled(bool* out_enabled);

  zx_status_t ClockSetRate(uint64_t hz);
  zx_status_t ClockQuerySupportedRate(uint64_t max_rate, uint64_t* out_max_supported_rate);
  zx_status_t ClockGetRate(uint64_t* out_current_rate);

  zx_status_t ClockSetInput(uint32_t idx);
  zx_status_t ClockGetNumInputs(uint32_t* out);
  zx_status_t ClockGetInput(uint32_t* out);

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

  const ddk::ClockImplProtocolClient clock_;
  const uint32_t id_;

  async_dispatcher_t* dispatcher_{fdf::Dispatcher::GetCurrent()->async_dispatcher()};
  component::OutgoingDirectory outgoing_{dispatcher_};
  fidl::ServerBindingGroup<fuchsia_hardware_clock::Clock> bindings_;
};

#endif  // SRC_DEVICES_CLOCK_DRIVERS_CLOCK_CLOCK_H_
