// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_INTERNAL_DRIVERS_FRAGMENT_FRAGMENT_H_
#define SRC_DEVICES_INTERNAL_DRIVERS_FRAGMENT_FRAGMENT_H_

#include <fuchsia/hardware/audio/cpp/banjo.h>
#include <fuchsia/hardware/camera/sensor/cpp/banjo.h>
#include <fuchsia/hardware/dsiimpl/cpp/banjo.h>
#include <fuchsia/hardware/gdc/cpp/banjo.h>
#include <fuchsia/hardware/ge2d/cpp/banjo.h>
#include <fuchsia/hardware/gpio/cpp/banjo.h>
#include <fuchsia/hardware/isp/cpp/banjo.h>
#include <fuchsia/hardware/mipicsi/cpp/banjo.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <fuchsia/hardware/powerimpl/cpp/banjo.h>
#include <fuchsia/hardware/scpi/cpp/banjo.h>
#include <fuchsia/hardware/sdio/cpp/banjo.h>
#include <fuchsia/hardware/shareddma/cpp/banjo.h>
#include <fuchsia/hardware/spi/cpp/banjo.h>
#include <fuchsia/hardware/sysmem/cpp/banjo.h>
#include <fuchsia/hardware/thermal/cpp/banjo.h>
#include <fuchsia/hardware/usb/phy/cpp/banjo.h>
#include <lib/async/cpp/wait.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/sync/completion.h>
#include <lib/zx/channel.h>

#include <ddktl/device.h>

namespace fragment {

template <typename ProtoClientType, typename ProtoType>
class ProtocolClient {
 public:
  ProtocolClient() { parent_ = nullptr; }
  ProtocolClient(zx_device_t* parent, uint32_t proto_id);
  ProtoClientType& proto_client() { return proto_client_; }

 private:
  ProtoType proto_;
  ProtoClientType proto_client_;
  zx_device_t* parent_;
};

class Fragment;
using FragmentBase =
    ddk::Device<Fragment, ddk::GetProtocolable, ddk::Initializable, ddk::Unbindable>;

class Fragment : public FragmentBase {
 public:
  explicit Fragment(zx_device_t* parent, async_dispatcher_t* dispatcher)
      : FragmentBase(parent),
        gpio_client_(parent, ZX_PROTOCOL_GPIO),
        dai_client_(parent, ZX_PROTOCOL_DAI),
        pdev_client_(parent, ZX_PROTOCOL_PDEV),
        spi_client_(parent, ZX_PROTOCOL_SPI),
        sysmem_client_(parent, ZX_PROTOCOL_SYSMEM),
        power_impl_client_(parent, ZX_PROTOCOL_POWER_IMPL),
        dsi_impl_client_(parent, ZX_PROTOCOL_DSI_IMPL),
        sdio_client_(parent, ZX_PROTOCOL_SDIO),
        thermal_client_(parent, ZX_PROTOCOL_THERMAL),
        isp_client_(parent, ZX_PROTOCOL_ISP),
        shared_dma_client_(parent, ZX_PROTOCOL_SHARED_DMA),
        usb_phy_client_(parent, ZX_PROTOCOL_USB_PHY),
        mipi_csi_client_(parent, ZX_PROTOCOL_MIPI_CSI),
        camera_sensor2_client_(parent, ZX_PROTOCOL_CAMERA_SENSOR2),
        gdc_client_(parent, ZX_PROTOCOL_GDC),
        ge2d_client_(parent, ZX_PROTOCOL_GE2D),
        scpi_client_(parent, ZX_PROTOCOL_SCPI),
        dispatcher_(dispatcher),
        outgoing_(dispatcher) {}

  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();
  void DdkUnbind(ddk::UnbindTxn txn);
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out_protocol);

 private:
  // This method should be called when there is a new message on `rpc_channel_`.
  // It will read and handle the FIDL message from the channel.
  zx_status_t ReadFidlFromChannel();

  struct CodecTransactContext {
    sync_completion_t completion;
    zx_status_t status;
    void* buffer;
    size_t size;
  };

  zx_status_t RpcGpio(const uint8_t* req_buf, uint32_t req_size, uint8_t* resp_buf,
                      uint32_t* out_resp_size, zx::handle* req_handles, uint32_t req_handle_count,
                      zx::handle* resp_handles, uint32_t* resp_handle_count);
  zx_status_t RpcPdev(const uint8_t* req_buf, uint32_t req_size, uint8_t* resp_buf,
                      uint32_t* out_resp_size, zx::handle* req_handles, uint32_t req_handle_count,
                      zx::handle* resp_handles, uint32_t* resp_handle_count);
  zx_status_t RpcSpi(const uint8_t* req_buf, uint32_t req_size, uint8_t* resp_buf,
                     uint32_t* out_resp_size, zx::handle* req_handles, uint32_t req_handle_count,
                     zx::handle* resp_handles, uint32_t* resp_handle_count);
  zx_status_t RpcSysmem(const uint8_t* req_buf, uint32_t req_size, uint8_t* resp_buf,
                        uint32_t* out_resp_size, zx::handle* req_handles, uint32_t req_handle_count,
                        zx::handle* resp_handles, uint32_t* resp_handle_count);
  zx_status_t RpcDai(const uint8_t* req_buf, uint32_t req_size, uint8_t* resp_buf,
                     uint32_t* out_resp_size, zx::handle* req_handles, uint32_t req_handle_count,
                     zx::handle* resp_handles, uint32_t* resp_handle_count);

  ProtocolClient<ddk::GpioProtocolClient, gpio_protocol_t> gpio_client_;
  ProtocolClient<ddk::DaiProtocolClient, dai_protocol_t> dai_client_;
  ProtocolClient<ddk::PDevProtocolClient, pdev_protocol_t> pdev_client_;
  ProtocolClient<ddk::SpiProtocolClient, spi_protocol_t> spi_client_;
  ProtocolClient<ddk::SysmemProtocolClient, sysmem_protocol_t> sysmem_client_;
  ProtocolClient<ddk::PowerImplProtocolClient, power_impl_protocol_t> power_impl_client_;
  ProtocolClient<ddk::DsiImplProtocolClient, dsi_impl_protocol_t> dsi_impl_client_;
  ProtocolClient<ddk::SdioProtocolClient, sdio_protocol_t> sdio_client_;
  ProtocolClient<ddk::ThermalProtocolClient, thermal_protocol_t> thermal_client_;
  ProtocolClient<ddk::IspProtocolClient, isp_protocol_t> isp_client_;
  ProtocolClient<ddk::SharedDmaProtocolClient, shared_dma_protocol_t> shared_dma_client_;
  ProtocolClient<ddk::UsbPhyProtocolClient, usb_phy_protocol_t> usb_phy_client_;
  ProtocolClient<ddk::MipiCsiProtocolClient, mipi_csi_protocol_t> mipi_csi_client_;
  ProtocolClient<ddk::CameraSensor2ProtocolClient, camera_sensor2_protocol_t>
      camera_sensor2_client_;
  ProtocolClient<ddk::GdcProtocolClient, gdc_protocol_t> gdc_client_;
  ProtocolClient<ddk::Ge2dProtocolClient, ge2d_protocol_t> ge2d_client_;
  ProtocolClient<ddk::ScpiProtocolClient, scpi_protocol_t> scpi_client_;

  async::Wait rpc_wait_;
  zx::channel rpc_channel_;
  async_dispatcher_t* dispatcher_;
  std::optional<component::OutgoingDirectory> outgoing_;
  fidl::ServerEnd<fuchsia_io::Directory> server_endpoint_;
};

}  // namespace fragment

#endif  // SRC_DEVICES_INTERNAL_DRIVERS_FRAGMENT_FRAGMENT_H_
