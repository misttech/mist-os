// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/ethernet/drivers/usb-cdc-function/cdc-eth-function.h"

#include <fidl/fuchsia.hardware.usb.endpoint/cpp/fidl.h>
#include <fidl/fuchsia.hardware.usb.function/cpp/fidl.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/trace/event.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <mutex>
#include <vector>

#include <usb-endpoint/usb-endpoint-client.h>
#include <usb/request-fidl.h>
#include <usb/usb-request.h>

namespace usb_cdc_function {

namespace fendpoint = fuchsia_hardware_usb_endpoint;
namespace ffunction = fuchsia_hardware_usb_function;
namespace frequest = fuchsia_hardware_usb_request;

typedef struct txn_info {
  ethernet_netbuf_t netbuf;
  ethernet_impl_queue_tx_callback completion_cb;
  void* cookie;
  list_node_t node;
} txn_info_t;

static void complete_txn(txn_info_t* txn, zx_status_t status) {
  txn->completion_cb(txn->cookie, status, &txn->netbuf);
}

zx_status_t UsbCdc::insert_usb_request(usb::FidlRequest&& req, usb::EndpointClient<UsbCdc>& ep) {
  if (suspend_txn_.has_value()) {
    return ZX_OK;
  }
  ep.PutRequest(std::move(req));
  return ZX_OK;
}

void UsbCdc::usb_request_queue(usb::FidlRequest&& req, usb::EndpointClient<UsbCdc>& ep) {
  if (suspend_txn_.has_value()) {
    return;
  }

  std::vector<frequest::Request> reqs;
  reqs.emplace_back(req.take_request());
  auto result = ep->QueueRequests({std::move(reqs)});
  ZX_ASSERT(result.is_ok());
}

zx_status_t UsbCdc::cdc_generate_mac_address() {
  size_t actual;
  auto status = device_get_metadata(parent_, DEVICE_METADATA_MAC_ADDRESS, &mac_addr_,
                                    sizeof(mac_addr_), &actual);
  if (status != ZX_OK || actual != sizeof(mac_addr_)) {
    zxlogf(WARNING, "CDC: MAC address metadata not found. Generating random address");

    zx_cprng_draw(mac_addr_, sizeof(mac_addr_));
    mac_addr_[0] = 0x02;
  }

  char buffer[sizeof(mac_addr_) * 3];
  snprintf(buffer, sizeof(buffer), "%02X%02X%02X%02X%02X%02X", mac_addr_[0], mac_addr_[1],
           mac_addr_[2], mac_addr_[3], mac_addr_[4], mac_addr_[5]);

  // Make the host and device addresses different so packets are routed correctly.
  mac_addr_[5] ^= 1;

  return function_.AllocStringDesc(buffer, &descriptors_.cdc_eth.iMACAddress);
}

zx_status_t UsbCdc::EthernetImplQuery(uint32_t options, ethernet_info_t* out_info) {
  zxlogf(DEBUG, "%s:", __func__);

  // No options are supported
  if (options) {
    zxlogf(ERROR, "%s: unexpected options (0x%" PRIx32 ") to ethernet_impl_query", __func__,
           options);
    return ZX_ERR_INVALID_ARGS;
  }

  memset(out_info, 0, sizeof(*out_info));
  out_info->mtu = ETH_MTU;
  memcpy(out_info->mac, mac_addr_, sizeof(mac_addr_));
  out_info->netbuf_size = sizeof(txn_info_t);

  return ZX_OK;
}

void UsbCdc::EthernetImplStop() {
  zxlogf(DEBUG, "%s:", __func__);

  tx_mutex_->lock();
  ethernet_mutex_.lock();
  ethernet_ifc_.clear();
  ethernet_mutex_.unlock();
  tx_mutex_->unlock();
}

zx_status_t UsbCdc::EthernetImplStart(const ethernet_ifc_protocol_t* ifc) {
  zxlogf(DEBUG, "%s:", __func__);

  zx_status_t status = ZX_OK;
  if (unbound_) {
    return ZX_ERR_BAD_STATE;
  }
  ethernet_mutex_.lock();
  if (ethernet_ifc_.is_valid()) {
    status = ZX_ERR_ALREADY_BOUND;
  } else {
    ethernet_ifc_ = ddk::EthernetIfcProtocolClient(ifc);
    ethernet_ifc_.Status(online_ ? ETHERNET_STATUS_ONLINE : 0);
  }
  ethernet_mutex_.unlock();

  return status;
}

zx_status_t UsbCdc::cdc_send_locked(ethernet_netbuf_t* netbuf) {
  {
    std::lock_guard<std::mutex> _(ethernet_mutex_);
    if (!ethernet_ifc_.is_valid()) {
      return ZX_ERR_BAD_STATE;
    }
  }

  const auto* byte_data = static_cast<const uint8_t*>(netbuf->data_buffer);
  size_t length = netbuf->data_size;

  // Make sure that we can get all of the tx buffers we need to use
  std::optional<usb::FidlRequest> tx_req = bulk_in_ep_.GetRequest();
  if (!tx_req.has_value()) {
    return ZX_ERR_SHOULD_WAIT;
  }

  // Send data
  tx_req->clear_buffers();
  std::vector<size_t> actual = tx_req->CopyTo(0, byte_data, length, bulk_in_ep_.GetMappedLocked);

  size_t actual_total = 0;
  for (size_t i = 0; i < actual.size(); i++) {
    // Fill in size of data.
    (*tx_req)->data()->at(i).size(actual[i]);
    actual_total += actual[i];
  }

  if (actual_total != length) {
    insert_usb_request(std::move(tx_req.value()), bulk_in_ep_);
    return ZX_ERR_INTERNAL;
  }

  zx_status_t status = tx_req->CacheFlush(bulk_in_ep_.GetMappedLocked);
  if (status != ZX_OK) {
    zxlogf(ERROR, "tx_req->CacheFlush(): %s", zx_status_get_string(status));
    return status;
  }
  usb_request_queue(std::move(tx_req.value()), bulk_in_ep_);

  return ZX_OK;
}

void UsbCdc::EthernetImplQueueTx(uint32_t options, ethernet_netbuf_t* netbuf,
                                 ethernet_impl_queue_tx_callback callback, void* cookie) {
  size_t length = netbuf->data_size;
  zx_status_t status;

  txn_info_t* txn = containerof(netbuf, txn_info_t, netbuf);
  txn->completion_cb = callback;
  txn->cookie = cookie;

  {
    std::lock_guard<std::mutex> _(ethernet_mutex_);
    if (!online_ || length > ETH_MTU || length == 0 || unbound_) {
      complete_txn(txn, ZX_ERR_INVALID_ARGS);
      return;
    }
  }

  zxlogf(SERIAL, "%s: sending %zu bytes", __func__, length);

  tx_mutex_->lock();
  if (unbound_ || suspend_txn_.has_value()) {
    status = ZX_ERR_IO_NOT_PRESENT;
  } else {
    status = cdc_send_locked(netbuf);
    if (status == ZX_ERR_SHOULD_WAIT) {
      // No buffers available, queue it up
      txn_info_t* txn = containerof(netbuf, txn_info_t, netbuf);
      list_add_tail(tx_pending_infos(), &txn->node);
    }
  }

  tx_mutex_->unlock();
  if (status != ZX_ERR_SHOULD_WAIT) {
    complete_txn(txn, status);
  }
}

zx_status_t UsbCdc::EthernetImplSetParam(uint32_t param, int32_t value, const uint8_t* data_buffer,
                                         size_t data_size) {
  return ZX_ERR_NOT_SUPPORTED;
}

void UsbCdc::cdc_intr_complete(fendpoint::Completion completion) {
  usb::FidlRequest req{std::move(completion.request().value())};

  intr_mutex_->lock();
  if (!suspend_txn_.has_value()) {
    zx_status_t status = insert_usb_request(std::move(req), intr_ep_);
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }
  intr_mutex_->unlock();
}

void UsbCdc::cdc_send_notifications() {
  std::lock_guard<std::mutex> _(ethernet_mutex_);

  usb_cdc_notification_t network_notification = {
      .bmRequestType = USB_DIR_IN | USB_TYPE_CLASS | USB_RECIP_INTERFACE,
      .bNotification = USB_CDC_NC_NETWORK_CONNECTION,
      .wValue = online_,
      .wIndex = descriptors_.cdc_intf_0.b_interface_number,
      .wLength = 0,
  };

  usb_cdc_speed_change_notification_t speed_notification = {
      .notification =
          {
              .bmRequestType = USB_DIR_IN | USB_TYPE_CLASS | USB_RECIP_INTERFACE,
              .bNotification = USB_CDC_NC_CONNECTION_SPEED_CHANGE,
              .wValue = 0,
              .wIndex = descriptors_.cdc_intf_0.b_interface_number,
              .wLength = 2 * sizeof(uint32_t),
          },
      .downlink_br = 0,
      .uplink_br = 0,
  };

  if (online_) {
    if (speed_ == USB_SPEED_SUPER) {
      // Claim to be gigabit speed.
      speed_notification.downlink_br = speed_notification.uplink_br = 1000 * 1000 * 1000;
    } else {
      // Claim to be 100 megabit speed.
      speed_notification.downlink_br = speed_notification.uplink_br = 100 * 1000 * 1000;
    }
  } else {
    speed_notification.downlink_br = speed_notification.uplink_br = 0;
  }
  intr_mutex_->lock();
  std::optional<usb::FidlRequest> req = intr_ep_.GetRequest();
  intr_mutex_->unlock();
  if (!req.has_value()) {
    zxlogf(ERROR, "%s: no interrupt request available", __func__);
    return;
  }

  req->clear_buffers();
  std::vector<size_t> actual =
      req->CopyTo(0, &network_notification, sizeof(network_notification), intr_ep_.GetMapped);

  size_t actual_total = 0;
  for (size_t i = 0; i < actual.size(); i++) {
    // Fill in size of data.
    (*req)->data()->at(i).size(actual[i]);
    actual_total += actual[i];
  }

  ZX_ASSERT(actual_total == sizeof(network_notification));

  req->CacheFlush(intr_ep_.GetMapped);
  usb_request_queue(std::move(req.value()), intr_ep_);
  intr_mutex_->lock();
  std::optional<usb::FidlRequest> req2 = intr_ep_.GetRequest();
  intr_mutex_->unlock();
  if (!req2.has_value()) {
    zxlogf(ERROR, "%s: no interrupt request available", __func__);
    return;
  }

  req2->clear_buffers();
  actual = req2->CopyTo(0, &speed_notification, sizeof(speed_notification), intr_ep_.GetMapped);

  actual_total = 0;
  for (size_t i = 0; i < actual.size(); i++) {
    // Fill in size of data.
    (*req2)->data()->at(i).size(actual[i]);
    actual_total += actual[i];
  }

  ZX_ASSERT(actual_total == sizeof(speed_notification));

  req2->CacheFlush(intr_ep_.GetMapped);
  usb_request_queue(std::move(req2.value()), intr_ep_);
}

void UsbCdc::cdc_rx_complete(fendpoint::Completion completion) {
  usb::FidlRequest req{std::move(completion.request().value())};
  zx_status_t status = *completion.status();

  if (status == ZX_ERR_IO_NOT_PRESENT) {
    rx_mutex_->lock();
    zx_status_t status = insert_usb_request(std::move(req), bulk_out_ep_);
    ZX_DEBUG_ASSERT(status == ZX_OK);
    rx_mutex_->unlock();
    return;
  }
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: usb_read_complete called with status %s", __func__,
           zx_status_get_string(status));
  }

  if (status == ZX_OK) {
    ethernet_mutex_.lock();
    if (ethernet_ifc_.is_valid()) {
      std::optional<zx_vaddr_t> addr = bulk_out_ep_.GetMappedAddr(req.request(), 0);
      if (addr.has_value()) {
        ethernet_ifc_.Recv(reinterpret_cast<uint8_t*>(*addr), *completion.transfer_size(), 0);
      }
    }
    ethernet_mutex_.unlock();
  }

  req.reset_buffers(bulk_out_ep_.GetMapped);
  status = req.CacheFlushInvalidate(bulk_out_ep_.GetMapped);
  if (status != ZX_OK) {
    zxlogf(ERROR, "CacheFlushInvalidate(): %s", zx_status_get_string(status));
  }

  usb_request_queue(std::move(req), bulk_out_ep_);
}

void UsbCdc::cdc_tx_complete(fendpoint::Completion completion) {
  usb::FidlRequest req{std::move(completion.request().value())};

  if (unbound_) {
    return;
  }
  tx_mutex_->lock();
  {
    if (suspend_txn_.has_value()) {
      tx_mutex_->unlock();
      return;
    }
    zx_status_t status = insert_usb_request(std::move(req), bulk_in_ep_);
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }

  bool additional_tx_queued = false;
  txn_info_t* txn;
  zx_status_t send_status = ZX_OK;

  // Do not queue requests if status is ZX_ERR_IO_NOT_PRESENT, as the underlying connection could be
  // disconnected or USB_RESET is being processed. Calling cdc_send_locked in such scenario will
  // deadlock and crash the driver (see https://fxbug.dev/42174506).
  if (*completion.status() != ZX_ERR_IO_NOT_PRESENT) {
    if ((txn = list_peek_head_type(tx_pending_infos(), txn_info_t, node))) {
      if ((send_status = cdc_send_locked(&txn->netbuf)) != ZX_ERR_SHOULD_WAIT) {
        list_remove_head(tx_pending_infos());
        additional_tx_queued = true;
      }
    }
  }

  tx_mutex_->unlock();

  if (additional_tx_queued) {
    ethernet_mutex_.lock();
    complete_txn(txn, send_status);
    ethernet_mutex_.unlock();
  }
}

size_t UsbCdc::UsbFunctionInterfaceGetDescriptorsSize() { return sizeof(descriptors_); }

void UsbCdc::UsbFunctionInterfaceGetDescriptors(uint8_t* out_descriptors_buffer,
                                                size_t descriptors_size,
                                                size_t* out_descriptors_actual) {
  const size_t length = std::min(sizeof(descriptors_), descriptors_size);
  memcpy(out_descriptors_buffer, &descriptors_, length);
  *out_descriptors_actual = length;
}

zx_status_t UsbCdc::UsbFunctionInterfaceControl(const usb_setup_t* setup,
                                                const uint8_t* write_buffer, size_t write_size,
                                                uint8_t* out_read_buffer, size_t read_size,
                                                size_t* out_read_actual) {
  TRACE_DURATION("cdc_eth", __func__, "write_size", write_size, "read_size", read_size);
  if (out_read_actual != NULL) {
    *out_read_actual = 0;
  }

  zxlogf(DEBUG, "%s", __func__);

  // USB_CDC_SET_ETHERNET_PACKET_FILTER is the only control request required by the spec
  if (setup->bm_request_type == (USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE) &&
      setup->b_request == USB_CDC_SET_ETHERNET_PACKET_FILTER) {
    zxlogf(DEBUG, "%s: USB_CDC_SET_ETHERNET_PACKET_FILTER", __func__);
    // TODO(voydanoff) implement the requested packet filtering
    return ZX_OK;
  }

  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t UsbCdc::UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed) {
  TRACE_DURATION("cdc_eth", __func__, "configured", configured, "speed", speed);
  zxlogf(INFO, "%s %d %d", __func__, configured, speed);

  zx_status_t status;
  zxlogf(DEBUG, "%s: before crit_enter", __func__);
  ethernet_mutex_.lock();
  zxlogf(DEBUG, "%s: after crit_enter", __func__);
  online_ = false;
  if (ethernet_ifc_.is_valid()) {
    ethernet_ifc_.Status(0);
  }
  ethernet_mutex_.unlock();
  zxlogf(DEBUG, "%s: after crit_leave", __func__);

  if (configured) {
    if ((status = function_.ConfigEp(&descriptors_.intr_ep, NULL)) != ZX_OK) {
      zxlogf(ERROR, "%s: usb_function_config_ep failed", __func__);
      return status;
    }
    speed_ = speed;
    cdc_send_notifications();
  } else {
    function_.DisableEp(bulk_out_addr_);
    function_.DisableEp(bulk_in_addr_);
    function_.DisableEp(intr_addr_);
    speed_ = USB_SPEED_UNDEFINED;
  }

  zxlogf(DEBUG, "%s: return ZX_OK", __func__);
  return ZX_OK;
}

zx_status_t UsbCdc::UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting) {
  TRACE_DURATION("cdc_eth", __func__, "interface", interface, "alt_setting", alt_setting);
  zxlogf(INFO, "%s: %d %d", __func__, interface, alt_setting);

  zx_status_t status;
  if (interface != descriptors_.cdc_intf_0.b_interface_number || alt_setting > 1) {
    return ZX_ERR_INVALID_ARGS;
  }

  // TODO(voydanoff) fullspeed and superspeed support
  if (alt_setting) {
    if ((status = function_.ConfigEp(&descriptors_.bulk_out_ep, NULL)) != ZX_OK ||
        (status = function_.ConfigEp(&descriptors_.bulk_in_ep, NULL)) != ZX_OK) {
      zxlogf(ERROR, "%s: usb_function_config_ep failed", __func__);
    }
  } else {
    if ((status = function_.DisableEp(bulk_out_addr_)) != ZX_OK ||
        (status = function_.DisableEp(bulk_in_addr_)) != ZX_OK) {
      zxlogf(ERROR, "%s: usb_function_disable_ep failed", __func__);
    }
  }

  bool online = false;
  if (alt_setting && status == ZX_OK) {
    online = true;

    // queue our OUT reqs
    rx_mutex_->lock();
    while (!bulk_out_ep_.RequestsEmpty()) {
      std::optional<usb::FidlRequest> req = bulk_out_ep_.GetRequest();
      ZX_ASSERT(req.has_value());  // A given from the loop.
      req->reset_buffers(bulk_out_ep_.GetMappedLocked);

      status = req->CacheFlushInvalidate(bulk_out_ep_.GetMappedLocked);
      if (status != ZX_OK) {
        zxlogf(ERROR, "CacheFlushInvalidate(): %s", zx_status_get_string(status));
        rx_mutex_->unlock();
        return status;
      }
      usb_request_queue(std::move(req.value()), bulk_out_ep_);
    }
    rx_mutex_->unlock();
  }

  ethernet_mutex_.lock();
  online_ = online;
  if (ethernet_ifc_.is_valid()) {
    ethernet_ifc_.Status(online ? ETHERNET_STATUS_ONLINE : 0);
  }
  ethernet_mutex_.unlock();

  // send status notifications on interrupt endpoint
  cdc_send_notifications();

  return status;
}

void UsbCdc::DdkInit(ddk::InitTxn txn) {
  list_initialize(&tx_pending_infos_);

  auto status = function_.AllocInterface(&descriptors_.comm_intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: AllocInterface failed", __func__);
    txn.Reply(status);
    return;
  }
  status = function_.AllocInterface(&descriptors_.cdc_intf_0.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: AllocInterface failed", __func__);
    txn.Reply(status);
    return;
  }
  descriptors_.cdc_intf_1.b_interface_number = descriptors_.cdc_intf_0.b_interface_number;
  descriptors_.cdc_union.bControlInterface = descriptors_.comm_intf.b_interface_number;
  descriptors_.cdc_union.bSubordinateInterface = descriptors_.cdc_intf_0.b_interface_number;

  status = function_.AllocEp(USB_DIR_OUT, &bulk_out_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: AllocEp failed", __func__);
    txn.Reply(status);
    return;
  }
  status = function_.AllocEp(USB_DIR_IN, &bulk_in_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: AllocEp failed", __func__);
    txn.Reply(status);
    return;
  }
  status = function_.AllocEp(USB_DIR_IN, &intr_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: AllocEp failed", __func__);
    txn.Reply(status);
    return;
  }

  descriptors_.bulk_out_ep.b_endpoint_address = bulk_out_addr_;
  descriptors_.bulk_in_ep.b_endpoint_address = bulk_in_addr_;
  descriptors_.intr_ep.b_endpoint_address = intr_addr_;

  status = cdc_generate_mac_address();
  if (status != ZX_OK) {
    txn.Reply(status);
    return;
  }

  auto dispatcher =
      fdf::SynchronizedDispatcher::Create({}, "cdc-ep-dispatcher", [](fdf_dispatcher_t*) {});
  if (dispatcher.is_error()) {
    zxlogf(ERROR, "fdf::SynchronizedDispatcher::Create(): %s", dispatcher.status_string());
    txn.Reply(dispatcher.error_value());
    return;
  }
  dispatcher_ = std::move(dispatcher.value());

  auto result = DdkConnectFidlProtocol<ffunction::UsbFunctionService::Device>(parent());
  if (result.is_error()) {
    zxlogf(ERROR, "DdkConnectFidlProtocol(): %s\n", result.status_string());
    txn.Reply(result.error_value());
    return;
  }

  // allocate bulk out usb requests
  status = bulk_out_ep_.Init(bulk_out_addr_, result.value(), dispatcher_.async_dispatcher());
  if (status != ZX_OK) {
    zxlogf(ERROR, "bulk_out_ep_.Init(): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }

  size_t actual =
      bulk_out_ep_.AddRequests(BULK_RX_COUNT, BULK_REQ_SIZE, frequest::Buffer::Tag::kVmoId);
  if (actual != BULK_RX_COUNT) {
    zxlogf(ERROR, "bulk_out_ep_.AddRequests() returned %ld reqs", actual);
    txn.Reply(ZX_ERR_INTERNAL);
    return;
  }

  // allocate bulk in usb requests
  status = bulk_in_ep_.Init(bulk_in_addr_, result.value(), dispatcher_.async_dispatcher());
  if (status != ZX_OK) {
    zxlogf(ERROR, "bulk_in_ep_.Init(): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }

  actual = bulk_in_ep_.AddRequests(BULK_TX_COUNT, BULK_REQ_SIZE, frequest::Buffer::Tag::kVmoId);
  if (actual != BULK_TX_COUNT) {
    zxlogf(ERROR, "bulk_in_ep_.AddRequests() returned %ld reqs", actual);
    txn.Reply(ZX_ERR_INTERNAL);
    return;
  }

  // allocate interrupt requests
  status = intr_ep_.Init(intr_addr_, result.value(), dispatcher_.async_dispatcher());
  if (status != ZX_OK) {
    zxlogf(ERROR, "intr_ep_.Init(): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }

  actual = intr_ep_.AddRequests(INTR_COUNT, BULK_REQ_SIZE, frequest::Buffer::Tag::kVmoId);
  if (actual != INTR_COUNT) {
    zxlogf(ERROR, "intr_ep_.AddRequests() returned %ld reqs", actual);
    txn.Reply(ZX_ERR_INTERNAL);
    return;
  }

  status = function_.SetInterface(this, &usb_function_interface_protocol_ops_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "function_.SetInterface(): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }

  txn.Reply(ZX_OK);
}

void UsbCdc::DdkUnbind(ddk::UnbindTxn txn) {
  zxlogf(DEBUG, "%s", __func__);
  unbound_ = true;
  {
    std::lock_guard<std::mutex> l(*tx_mutex_);
    txn_info_t* txn;
    while ((txn = list_remove_head_type(tx_pending_infos(), txn_info_t, node)) != NULL) {
      complete_txn(txn, ZX_ERR_PEER_CLOSED);
    }
  }

  txn.Reply();
}

void UsbCdc::DdkRelease() {
  zxlogf(DEBUG, "%s", __func__);
  if (suspend_thread_.has_value()) {
    suspend_thread_->join();
  }

  dispatcher_.ShutdownAsync();
  dispatcher_.release();

  delete this;
}

void UsbCdc::DdkSuspend(ddk::SuspendTxn txn) {
  // Start the suspend process by setting the suspend txn
  // When the pipeline tries to submit requests, they will be immediately free'd.
  suspend_txn_.emplace(std::move(txn));
  suspend_thread_.emplace([this]() {
    // Disable endpoints to prevent new requests present in our
    // pipeline from getting queued.
    function_.DisableEp(bulk_out_addr_);
    function_.DisableEp(bulk_in_addr_);
    function_.DisableEp(intr_addr_);

    // Cancel all requests in the pipeline -- the completion handler
    // will free these requests as they come in.
    function_.CancelAll(intr_addr_);
    function_.CancelAll(bulk_out_addr_);
    function_.CancelAll(bulk_in_addr_);

    list_node_t list;
    {
      std::lock_guard<std::mutex> l(*tx_mutex_);
      list_move(tx_pending_infos(), &list);
    }
    txn_info_t* tx_txn;
    while ((tx_txn = list_remove_head_type(&list, txn_info_t, node)) != NULL) {
      complete_txn(tx_txn, ZX_ERR_PEER_CLOSED);
    }

    suspend_txn_->Reply(ZX_OK, 0);
  });
}

zx_status_t UsbCdc::Bind(void* ctx, zx_device_t* parent) {
  zxlogf(INFO, "%s", __func__);
  auto cdc = std::make_unique<UsbCdc>(parent);
  if (!cdc) {
    zxlogf(ERROR, "Could not create UsbCdc.");
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = cdc->DdkAdd("cdc-eth-function");

  // Either the DDK now owns this reference, or (in the case of failure) it will be freed in the
  // block below. In either case, we want to avoid the unique_ptr destructing the allocated
  // UsbCdc instance.
  [[maybe_unused]] auto released = cdc.release();

  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not add UsbCdc %s.", zx_status_get_string(status));
    cdc->DdkRelease();
    return status;
  }

  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = UsbCdc::Bind;
  return ops;
}();

}  // namespace usb_cdc_function

// clang-format off
ZIRCON_DRIVER(usb_cdc, usb_cdc_function::driver_ops, "zircon", "0.1");
