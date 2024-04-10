// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_ETHERNET_DRIVERS_USB_CDC_FUNCTION_CDC_ETH_FUNCTION_H_
#define SRC_CONNECTIVITY_ETHERNET_DRIVERS_USB_CDC_FUNCTION_CDC_ETH_FUNCTION_H_

#include <endian.h>
#include <fuchsia/hardware/ethernet/cpp/banjo.h>
#include <fuchsia/hardware/usb/function/cpp/banjo.h>
#include <lib/ddk/device.h>
#include <lib/sync/completion.h>
#include <zircon/listnode.h>

#include <atomic>
#include <optional>
#include <thread>

#include <ddktl/device.h>
#include <fbl/mutex.h>
#include <usb/cdc.h>
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
using UsbCdcType = ddk::Device<UsbCdc, ddk::Unbindable>;

class UsbCdc : public UsbCdcType,
               public ddk::EthernetImplProtocol<UsbCdc, ddk::base_protocol>,
               public ddk::UsbFunctionInterfaceProtocol<UsbCdc> {
 public:
  explicit UsbCdc(zx_device_t* parent) : UsbCdcType(parent), function_(parent) {}

  // Driver bind method.
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

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

  zx_status_t instrumented_request_alloc(usb_request_t** out, uint64_t data_size,
                                         uint8_t ep_address, size_t req_size);
  void instrumented_request_release(usb_request_t* req);
  zx_status_t insert_usb_request(list_node_t* list, usb_request_t* req, size_t parent_req_size,
                                 bool tail = true);
  static void usb_request_callback(void* ctx, usb_request_t* req);
  void usb_request_queue(usb_request_t* req, const usb_request_complete_callback_t* completion);
  zx_status_t cdc_generate_mac_address();
  zx_status_t cdc_send_locked(ethernet_netbuf_t* netbuf) __TA_REQUIRES(tx_mutex_);
  static void cdc_intr_complete(void* ctx, usb_request_t* req);
  void cdc_send_notifications();
  static void cdc_rx_complete(void* ctx, usb_request_t* req);
  static void cdc_tx_complete(void* ctx, usb_request_t* req);

  ddk::UsbFunctionProtocolClient function_;

  list_node_t bulk_out_reqs_ __TA_GUARDED(rx_mutex_) = {};     // list of usb_request_t
  list_node_t bulk_in_reqs_ __TA_GUARDED(tx_mutex_) = {};      // list of usb_request_t
  list_node_t intr_reqs_ __TA_GUARDED(intr_mutex_) = {};       // list of usb_request_t
  list_node_t tx_pending_infos_ __TA_GUARDED(tx_mutex_) = {};  // list of ethernet_netbuf_t

  // Use these methods to access the request lists defined above. These have correct thread
  // annotations and will ensure that any error in locking are caught during compilation.
  inline list_node_t* bulk_out_reqs() __TA_REQUIRES(rx_mutex_) { return &bulk_out_reqs_; }
  inline list_node_t* bulk_in_reqs() __TA_REQUIRES(tx_mutex_) { return &bulk_in_reqs_; }
  inline list_node_t* intr_reqs() __TA_REQUIRES(intr_mutex_) { return &intr_reqs_; }
  inline list_node_t* tx_pending_infos() __TA_REQUIRES(tx_mutex_) { return &tx_pending_infos_; }

  std::atomic_bool unbound_ = false;  // set to true when device is going away.

  // Device attributes
  uint8_t mac_addr_[ETH_MAC_SIZE] = {};
  // Ethernet lock -- must be acquired after tx_mutex
  // when both locks are held.
  mtx_t ethernet_mutex_ __TA_ACQUIRED_AFTER(tx_mutex_) = {};
  ddk::EthernetIfcProtocolClient ethernet_ifc_ __TA_GUARDED(ethernet_mutex_);
  bool online_ __TA_GUARDED(ethernet_mutex_) = false;
  usb_speed_t speed_ = 0;
  // TX lock -- Must be acquired before ethernet_mutex
  // when both locks are held.
  mtx_t tx_mutex_ = {};
  mtx_t rx_mutex_ = {};
  mtx_t intr_mutex_ = {};

  uint8_t bulk_out_addr_ = 0;
  uint8_t bulk_in_addr_ = 0;
  uint8_t intr_addr_ = 0;
  uint16_t bulk_max_packet_ = 0;

  size_t parent_req_size_ = 0;
  mtx_t pending_request_lock_ = {};
  cnd_t pending_requests_completed_ __TA_GUARDED(pending_request_lock_) = {};
  std::atomic_int32_t pending_request_count_ __TA_GUARDED(pending_request_lock_);
  std::atomic_int32_t allocated_requests_count_;
  sync_completion_t requests_freed_completion_;
  size_t usb_request_offset_ = 0;
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
