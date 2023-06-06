// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_INTERNAL_DRIVERS_FRAGMENT_FRAGMENT_PROXY_H_
#define SRC_DEVICES_INTERNAL_DRIVERS_FRAGMENT_FRAGMENT_PROXY_H_

#include <fuchsia/hardware/audio/cpp/banjo.h>
#include <fuchsia/hardware/clock/cpp/banjo.h>
#include <fuchsia/hardware/ethernet/board/cpp/banjo.h>
#include <fuchsia/hardware/gpio/cpp/banjo.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <fuchsia/hardware/spi/cpp/banjo.h>
#include <fuchsia/hardware/sysmem/cpp/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/zx/channel.h>

#include <ddktl/device.h>

#include "proxy-protocol.h"

namespace fragment {

class FragmentProxy;
using FragmentProxyBase = ddk::Device<FragmentProxy, ddk::GetProtocolable>;

class FragmentProxy : public FragmentProxyBase,
                      public ddk::ClockProtocol<FragmentProxy>,
                      public ddk::EthBoardProtocol<FragmentProxy>,
                      public ddk::GpioProtocol<FragmentProxy>,
                      public ddk::DaiProtocol<FragmentProxy>,
                      public ddk::PDevProtocol<FragmentProxy>,
                      public ddk::SpiProtocol<FragmentProxy>,
                      public ddk::SysmemProtocol<FragmentProxy> {
 public:
  FragmentProxy(zx_device_t* parent, zx::channel rpc)
      : FragmentProxyBase(parent), rpc_(std::move(rpc)) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  zx_status_t DdkGetProtocol(uint32_t, void*);
  void DdkRelease();

  zx_status_t Rpc(const ProxyRequest* req, size_t req_length, ProxyResponse* resp,
                  size_t resp_length, const zx_handle_t* in_handles, size_t in_handle_count,
                  zx_handle_t* out_handles, size_t out_handle_count, size_t* out_actual);

  zx_status_t Rpc(const ProxyRequest* req, size_t req_length, ProxyResponse* resp,
                  size_t resp_length) {
    return Rpc(req, req_length, resp, resp_length, nullptr, 0, nullptr, 0, nullptr);
  }

  void AcpiConnectServer(zx::channel server);

  zx_status_t ClockEnable();
  zx_status_t ClockDisable();
  zx_status_t ClockIsEnabled(bool* out_enabled);
  zx_status_t ClockSetRate(uint64_t hz);
  zx_status_t ClockQuerySupportedRate(uint64_t max_rate, uint64_t* out_max_supported_rate);
  zx_status_t ClockGetRate(uint64_t* out_current_rate);
  zx_status_t ClockSetInput(uint32_t idx);
  zx_status_t ClockGetNumInputs(uint32_t* out_num_inputs);
  zx_status_t ClockGetInput(uint32_t* out_current_input);
  zx_status_t EthBoardResetPhy();
  zx_status_t GpioConfigIn(uint32_t flags);
  zx_status_t GpioConfigOut(uint8_t initial_value);
  zx_status_t GpioSetAltFunction(uint64_t function);
  zx_status_t GpioRead(uint8_t* out_value);
  zx_status_t GpioWrite(uint8_t value);
  zx_status_t GpioGetInterrupt(uint32_t flags, zx::interrupt* out_irq);
  zx_status_t GpioReleaseInterrupt();
  zx_status_t GpioSetPolarity(gpio_polarity_t polarity);
  zx_status_t GpioSetDriveStrength(uint64_t ds_ua, uint64_t* out_actual_ds_ua);
  zx_status_t GpioGetDriveStrength(uint64_t* ds_ua);
  zx_status_t PDevGetMmio(uint32_t index, pdev_mmio_t* out_mmio);
  zx_status_t PDevGetInterrupt(uint32_t index, uint32_t flags, zx::interrupt* out_irq);
  zx_status_t PDevGetBti(uint32_t index, zx::bti* out_bti);
  zx_status_t PDevGetSmc(uint32_t index, zx::resource* out_smc);
  zx_status_t PDevGetDeviceInfo(pdev_device_info_t* out_info);
  zx_status_t PDevGetBoardInfo(pdev_board_info_t* out_info);
  zx_status_t PDevDeviceAdd(uint32_t index, const device_add_args_t* args,
                            zx_device_t** out_device);
  zx_status_t PDevGetProtocol(uint32_t proto_id, uint32_t index, void* out_out_protocol_buffer,
                              size_t out_protocol_size, size_t* out_out_protocol_actual);
  zx_status_t SpiTransmit(const uint8_t* txdata_list, size_t txdata_count);
  zx_status_t SpiReceive(uint32_t size, uint8_t* out_rxdata_list, size_t rxdata_count,
                         size_t* out_rxdata_actual);
  zx_status_t SpiExchange(const uint8_t* txdata_list, size_t txdata_count, uint8_t* out_rxdata_list,
                          size_t rxdata_count, size_t* out_rxdata_actual);
  zx_status_t SysmemConnect(zx::channel allocator2_request);
  zx_status_t SysmemRegisterHeap(uint64_t heap, zx::channel heap_connection);
  zx_status_t SysmemRegisterSecureMem(zx::channel tee_connection);
  zx_status_t SysmemUnregisterSecureMem();

  zx_status_t DaiConnect(zx::channel chan);

 private:
  zx::channel rpc_;
};

}  // namespace fragment

#endif  // SRC_DEVICES_INTERNAL_DRIVERS_FRAGMENT_FRAGMENT_PROXY_H_
