// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_ETHERNET_DRIVERS_USB_CDC_FUNCTION_CDC_ETH_FUNCTION_H_
#define SRC_CONNECTIVITY_ETHERNET_DRIVERS_USB_CDC_FUNCTION_CDC_ETH_FUNCTION_H_

#include <endian.h>
#include <fidl/fuchsia.hardware.usb.endpoint/cpp/fidl.h>
#include <fuchsia/hardware/ethernet/cpp/banjo.h>
#include <fuchsia/hardware/usb/function/cpp/banjo.h>
#include <lib/ddk/device.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/sync/completion.h>
#include <zircon/listnode.h>

#include <atomic>
#include <mutex>
#include <optional>
#include <thread>

#include <ddktl/device.h>
#include <usb-endpoint/usb-endpoint-client.h>
#include <usb/cdc.h>
#include <usb/request-fidl.h>
#include <usb/usb.h>

namespace usb_cdc_function {

#define BULK_REQ_SIZE 2048
#define BULK_TX_COUNT 16
#define BULK_RX_COUNT 16
#define INTR_COUNT 8

#define BULK_MAX_PACKET 512  // FIXME(voydanoff) USB 3.0 support
#define INTR_MAX_PACKET sizeof(usb_cdc_speed_change_notification_t)

#define ETH_MTU 1500

class UsbCdc;
using UsbCdcType = ddk::Device<UsbCdc, ddk::Initializable, ddk::Unbindable>;

class UsbCdc : public UsbCdcType,
               public ddk::EthernetImplProtocol<UsbCdc, ddk::base_protocol>,
               public ddk::UsbFunctionInterfaceProtocol<UsbCdc> {
 public:
  explicit UsbCdc(zx_device_t* parent) : UsbCdcType(parent), function_(parent) {}

  // Driver bind method.
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();
  void DdkSuspend(ddk::SuspendTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);

  // EthernetImpl methods.
  zx_status_t EthernetImplQuery(uint32_t options, ethernet_info_t* out_info);
  void EthernetImplStop();
  zx_status_t EthernetImplStart(const ethernet_ifc_protocol_t* ifc);
  void EthernetImplQueueTx(uint32_t options, ethernet_netbuf_t* netbuf,
                           ethernet_impl_queue_tx_callback callback, void* cookie);
  zx_status_t EthernetImplSetParam(uint32_t param, int32_t value, const uint8_t* data_buffer,
                                   size_t data_size);
  void EthernetImplGetBti(zx::bti* out_bti) { ZX_ASSERT(false); }

  // UsbFunctionInterface methods.
  size_t UsbFunctionInterfaceGetDescriptorsSize();
  void UsbFunctionInterfaceGetDescriptors(uint8_t* out_descriptors_buffer, size_t descriptors_size,
                                          size_t* out_descriptors_actual);
  zx_status_t UsbFunctionInterfaceControl(const usb_setup_t* setup, const uint8_t* write_buffer,
                                          size_t write_size, uint8_t* out_read_buffer,
                                          size_t read_size, size_t* out_read_actual);
  zx_status_t UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed);
  zx_status_t UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting);

  zx_status_t insert_usb_request(usb::FidlRequest&& req, usb::EndpointClient<UsbCdc>& ep);

  void usb_request_queue(usb::FidlRequest&& req, usb::EndpointClient<UsbCdc>& ep);

  zx_status_t cdc_generate_mac_address();
  zx_status_t cdc_send_locked(ethernet_netbuf_t* netbuf) __TA_REQUIRES(tx_mutex_);
  void cdc_intr_complete(fuchsia_hardware_usb_endpoint::Completion completion);
  void cdc_send_notifications();
  void cdc_rx_complete(fuchsia_hardware_usb_endpoint::Completion completion);
  void cdc_tx_complete(fuchsia_hardware_usb_endpoint::Completion completion);

  ddk::UsbFunctionProtocolClient function_;

  fdf::SynchronizedDispatcher dispatcher_;

  // In-direction (TX to host).
  usb::EndpointClient<UsbCdc> intr_ep_{usb::EndpointType::INTERRUPT, this,
                                       std::mem_fn(&UsbCdc::cdc_intr_complete)};

  // Out-direction (RX from host).
  usb::EndpointClient<UsbCdc> bulk_out_ep_{usb::EndpointType::BULK, this,
                                           std::mem_fn(&UsbCdc::cdc_rx_complete)};

  // In-direction (TX to host).
  usb::EndpointClient<UsbCdc> bulk_in_ep_{usb::EndpointType::BULK, this,
                                          std::mem_fn(&UsbCdc::cdc_tx_complete)};

  list_node_t tx_pending_infos_ __TA_GUARDED(tx_mutex_) = {};  // list of ethernet_netbuf_t

  // Use this method to access the request lists defined above. These have correct thread
  // annotations and will ensure that any error in locking are caught during compilation.
  list_node_t* tx_pending_infos() __TA_REQUIRES(tx_mutex_) { return &tx_pending_infos_; }

  std::atomic_bool unbound_ = false;  // set to true when device is going away.

  // Device attributes
  uint8_t mac_addr_[ETH_MAC_SIZE] = {};
  // Ethernet lock -- must be acquired after tx_mutex
  // when both locks are held.
  std::mutex ethernet_mutex_ __TA_ACQUIRED_AFTER(tx_mutex_);
  ddk::EthernetIfcProtocolClient ethernet_ifc_ __TA_GUARDED(ethernet_mutex_);
  bool online_ __TA_GUARDED(ethernet_mutex_) = false;
  bool configured_ = false;
  usb_speed_t speed_ = 0;
  // TX lock -- Must be acquired before ethernet_mutex
  // when both locks are held.
  std::mutex& tx_mutex_ = bulk_in_ep_.mutex();
  std::mutex& rx_mutex_ = bulk_out_ep_.mutex();
  std::mutex& intr_mutex_ = intr_ep_.mutex();

  uint8_t bulk_out_addr_ = 0;
  uint8_t bulk_in_addr_ = 0;
  uint8_t intr_addr_ = 0;

  std::optional<std::thread> suspend_thread_;
  std::optional<ddk::SuspendTxn> suspend_txn_;

  struct {
    usb_interface_descriptor_t comm_intf;
    usb_cs_header_interface_descriptor_t cdc_header;
    usb_cs_union_interface_descriptor_1_t cdc_union;
    usb_cs_ethernet_interface_descriptor_t cdc_eth;
    usb_endpoint_descriptor_t intr_ep;
    usb_interface_descriptor_t cdc_intf_0;
    usb_interface_descriptor_t cdc_intf_1;
    usb_endpoint_descriptor_t bulk_out_ep;
    usb_endpoint_descriptor_t bulk_in_ep;
  } descriptors_ = {
      .comm_intf =
          {
              .b_length = sizeof(usb_interface_descriptor_t),
              .b_descriptor_type = USB_DT_INTERFACE,
              .b_interface_number = 0,  // set later
              .b_alternate_setting = 0,
              .b_num_endpoints = 1,
              .b_interface_class = USB_CLASS_COMM,
              .b_interface_sub_class = USB_CDC_SUBCLASS_ETHERNET,
              .b_interface_protocol = 0,
              .i_interface = 0,
          },
      .cdc_header =
          {
              .bLength = sizeof(usb_cs_header_interface_descriptor_t),
              .bDescriptorType = USB_DT_CS_INTERFACE,
              .bDescriptorSubType = USB_CDC_DST_HEADER,
              .bcdCDC = 0x120,
          },
      .cdc_union =
          {
              .bLength = sizeof(usb_cs_union_interface_descriptor_1_t),
              .bDescriptorType = USB_DT_CS_INTERFACE,
              .bDescriptorSubType = USB_CDC_DST_UNION,
              .bControlInterface = 0,      // set later
              .bSubordinateInterface = 0,  // set later
          },
      .cdc_eth =
          {
              .bLength = sizeof(usb_cs_ethernet_interface_descriptor_t),
              .bDescriptorType = USB_DT_CS_INTERFACE,
              .bDescriptorSubType = USB_CDC_DST_ETHERNET,
              .iMACAddress = 0,  // set later
              .bmEthernetStatistics = 0,
              .wMaxSegmentSize = ETH_MTU,
              .wNumberMCFilters = 0,
              .bNumberPowerFilters = 0,
          },
      .intr_ep =
          {
              .b_length = sizeof(usb_endpoint_descriptor_t),
              .b_descriptor_type = USB_DT_ENDPOINT,
              .b_endpoint_address = 0,  // set later
              .bm_attributes = USB_ENDPOINT_INTERRUPT,
              .w_max_packet_size = htole16(INTR_MAX_PACKET),
              .b_interval = 8,
          },
      .cdc_intf_0 =
          {
              .b_length = sizeof(usb_interface_descriptor_t),
              .b_descriptor_type = USB_DT_INTERFACE,
              .b_interface_number = 0,  // set later
              .b_alternate_setting = 0,
              .b_num_endpoints = 0,
              .b_interface_class = USB_CLASS_CDC,
              .b_interface_sub_class = 0,
              .b_interface_protocol = 0,
              .i_interface = 0,
          },
      .cdc_intf_1 =
          {
              .b_length = sizeof(usb_interface_descriptor_t),
              .b_descriptor_type = USB_DT_INTERFACE,
              .b_interface_number = 0,  // set later
              .b_alternate_setting = 1,
              .b_num_endpoints = 2,
              .b_interface_class = USB_CLASS_CDC,
              .b_interface_sub_class = 0,
              .b_interface_protocol = 0,
              .i_interface = 0,
          },
      .bulk_out_ep =
          {
              .b_length = sizeof(usb_endpoint_descriptor_t),
              .b_descriptor_type = USB_DT_ENDPOINT,
              .b_endpoint_address = 0,  // set later
              .bm_attributes = USB_ENDPOINT_BULK,
              .w_max_packet_size = htole16(BULK_MAX_PACKET),
              .b_interval = 0,
          },
      .bulk_in_ep =
          {
              .b_length = sizeof(usb_endpoint_descriptor_t),
              .b_descriptor_type = USB_DT_ENDPOINT,
              .b_endpoint_address = 0,  // set later
              .bm_attributes = USB_ENDPOINT_BULK,
              .w_max_packet_size = htole16(BULK_MAX_PACKET),
              .b_interval = 0,
          },
  };
};

}  // namespace usb_cdc_function

#endif  // SRC_CONNECTIVITY_ETHERNET_DRIVERS_USB_CDC_FUNCTION_CDC_ETH_FUNCTION_H_
