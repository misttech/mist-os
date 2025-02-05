// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/ethernet/drivers/usb-cdc-function/cdc-eth-function.h"

#include <endian.h>
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
    zxlogf(INFO, "ethernet MAC metadata not found. Generating random address");

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
  // No options are supported
  if (options) {
    zxlogf(ERROR, "unexpected options (0x%" PRIx32 ") to ethernet_impl_query", options);
    return ZX_ERR_INVALID_ARGS;
  }

  memset(out_info, 0, sizeof(*out_info));
  out_info->mtu = ETH_MTU;
  memcpy(out_info->mac, mac_addr_, sizeof(mac_addr_));
  out_info->netbuf_size = sizeof(txn_info_t);

  return ZX_OK;
}

void UsbCdc::EthernetImplStop() {
  std::lock_guard<std::mutex> tx(tx_mutex_);
  std::lock_guard<std::mutex> ethernet(ethernet_mutex_);
  ethernet_ifc_.clear();
}

zx_status_t UsbCdc::EthernetImplStart(const ethernet_ifc_protocol_t* ifc) {
  if (unbound_) {
    return ZX_ERR_BAD_STATE;
  }
  std::lock_guard<std::mutex> ethernet(ethernet_mutex_);
  if (ethernet_ifc_.is_valid()) {
    return ZX_ERR_ALREADY_BOUND;
  }
  ethernet_ifc_ = ddk::EthernetIfcProtocolClient(ifc);
  ethernet_ifc_.Status(online_ ? ETHERNET_STATUS_ONLINE : 0);
  return ZX_OK;
}

zx_status_t UsbCdc::cdc_send_locked(ethernet_netbuf_t* netbuf) {
  {
    std::lock_guard<std::mutex> _(ethernet_mutex_);
    if (!ethernet_ifc_.is_valid()) {
      return ZX_ERR_BAD_STATE;
    }
  }

  const auto* byte_data = netbuf->data_buffer;
  size_t length = netbuf->data_size;

  // Make sure that we can get all of the tx buffers we need to use
  std::optional<usb::FidlRequest> tx_req = bulk_in_ep_.GetRequest();
  if (!tx_req.has_value()) {
    return ZX_ERR_SHOULD_WAIT;
  }

  // Send data
  tx_req->clear_buffers();
  std::vector<size_t> actual = tx_req->CopyTo(0, byte_data, length, bulk_in_ep_.GetMappedLocked());

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

  zx_status_t status = tx_req->CacheFlush(bulk_in_ep_.GetMappedLocked());
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] tx_req->CacheFlush(): %s", zx_status_get_string(status));
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

  {
    std::lock_guard<std::mutex> tx(tx_mutex_);
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
  }

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

  std::lock_guard<std::mutex> intr(intr_mutex_);
  if (!suspend_txn_.has_value()) {
    zx_status_t status = insert_usb_request(std::move(req), intr_ep_);
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }
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
  std::optional<usb::FidlRequest> req = intr_ep_.GetRequest();
  if (!req.has_value()) {
    zxlogf(ERROR, "[bug] intr_ep_.GetRequest(): no request available");
    return;
  }

  req->clear_buffers();
  std::vector<size_t> actual =
      req->CopyTo(0, &network_notification, sizeof(network_notification), intr_ep_.GetMapped());

  size_t actual_total = 0;
  for (size_t i = 0; i < actual.size(); i++) {
    // Fill in size of data.
    (*req)->data()->at(i).size(actual[i]);
    actual_total += actual[i];
  }

  ZX_ASSERT(actual_total == sizeof(network_notification));

  req->CacheFlush(intr_ep_.GetMapped());
  usb_request_queue(std::move(req.value()), intr_ep_);
  std::optional<usb::FidlRequest> req2 = intr_ep_.GetRequest();
  if (!req2.has_value()) {
    zxlogf(ERROR, "[bug] intr_ep_.GetRequest(): no request available");
    return;
  }

  req2->clear_buffers();
  actual = req2->CopyTo(0, &speed_notification, sizeof(speed_notification), intr_ep_.GetMapped());

  actual_total = 0;
  for (size_t i = 0; i < actual.size(); i++) {
    // Fill in size of data.
    (*req2)->data()->at(i).size(actual[i]);
    actual_total += actual[i];
  }

  ZX_ASSERT(actual_total == sizeof(speed_notification));

  req2->CacheFlush(intr_ep_.GetMapped());
  usb_request_queue(std::move(req2.value()), intr_ep_);
}

void UsbCdc::cdc_rx_complete(fendpoint::Completion completion) {
  usb::FidlRequest req{std::move(completion.request().value())};
  zx_status_t status = *completion.status();

  if (status == ZX_ERR_IO_NOT_PRESENT) {
    std::lock_guard<std::mutex> rx(rx_mutex_);
    zx_status_t status = insert_usb_request(std::move(req), bulk_out_ep_);
    ZX_DEBUG_ASSERT(status == ZX_OK);
    return;
  }
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] rx_completion: %s", zx_status_get_string(status));
  }

  if (status == ZX_OK) {
    std::lock_guard<std::mutex> ethernet(ethernet_mutex_);
    if (ethernet_ifc_.is_valid()) {
      std::optional<zx_vaddr_t> addr = bulk_out_ep_.GetMappedAddr(req.request(), 0);
      if (addr.has_value()) {
        ethernet_ifc_.Recv(reinterpret_cast<uint8_t*>(*addr), *completion.transfer_size(), 0);
      }
    }
  }

  req.reset_buffers(bulk_out_ep_.GetMapped());
  status = req.CacheFlushInvalidate(bulk_out_ep_.GetMapped());
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] CacheFlushInvalidate(): %s", zx_status_get_string(status));
  }

  usb_request_queue(std::move(req), bulk_out_ep_);
}

void UsbCdc::cdc_tx_complete(fendpoint::Completion completion) {
  usb::FidlRequest req{std::move(completion.request().value())};

  if (unbound_) {
    return;
  }

  std::optional additional_tx_queued =
      [&]() -> std::optional<std::tuple<txn_info_t*, zx_status_t>> {
    std::lock_guard<std::mutex> tx(tx_mutex_);
    {
      if (suspend_txn_.has_value()) {
        return std::nullopt;
      }
      zx_status_t status = insert_usb_request(std::move(req), bulk_in_ep_);
      ZX_DEBUG_ASSERT(status == ZX_OK);
    }

    // Do not queue requests if status is ZX_ERR_IO_NOT_PRESENT, as the underlying connection could
    // be disconnected or USB_RESET is being processed. Calling cdc_send_locked in such scenario
    // will deadlock and crash the driver (see https://fxbug.dev/42174506).
    if (*completion.status() != ZX_ERR_IO_NOT_PRESENT) {
      if (txn_info_t* txn = list_peek_head_type(tx_pending_infos(), txn_info_t, node);
          txn != nullptr) {
        if (zx_status_t send_status = cdc_send_locked(&txn->netbuf);
            send_status != ZX_ERR_SHOULD_WAIT) {
          list_remove_head(tx_pending_infos());
          return std::make_tuple(txn, send_status);
        }
      }
    }
    return std::nullopt;
  }();

  if (additional_tx_queued.has_value()) {
    std::lock_guard<std::mutex> ethernet(ethernet_mutex_);
    auto [txn, send_status] = *additional_tx_queued;
    complete_txn(txn, send_status);
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
  uint16_t w_value{le16toh(setup->w_value)};
  uint16_t w_index{le16toh(setup->w_index)};
  uint16_t w_length{le16toh(setup->w_length)};

  zxlogf(DEBUG,
         "bmRequestType=%02x bRequest=%02x wValue=%04x (%d) wIndex=%04x (%d) wLength=%04x (%d)",
         setup->bm_request_type, setup->b_request, w_value, w_value, w_index, w_index, w_length,
         w_length);

  TRACE_DURATION("cdc_eth", __func__, "write_size", write_size, "read_size", read_size);
  if (out_read_actual != NULL) {
    *out_read_actual = 0;
  }

  // The following control requests are currently unsupported, though non-criticial. To avoid
  // hanging up bus-enumeration, reply with success.

  if (setup->bm_request_type == (USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE) &&
      setup->b_request == USB_CDC_SET_ETHERNET_PACKET_FILTER) {
    zxlogf(DEBUG, "setting packet filter not supported");
    // TODO(voydanoff) implement the requested packet filtering
    return ZX_OK;
  }

  if (setup->bm_request_type == (USB_DIR_OUT | USB_TYPE_STANDARD | USB_RECIP_ENDPOINT) &&
      setup->b_request == USB_REQ_CLEAR_FEATURE && setup->w_value == USB_ENDPOINT_HALT) {
    zxlogf(DEBUG, "clearing endpoint-halt not supported");
    return ZX_OK;
  }

  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t UsbCdc::UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed) {
  TRACE_DURATION("cdc_eth", __func__, "configured", configured, "speed", speed);

  if (configured_) {
    return ZX_OK;
  }

  {
    std::lock_guard<std::mutex> ethernet(ethernet_mutex_);
    online_ = false;
    if (ethernet_ifc_.is_valid()) {
      ethernet_ifc_.Status(0);
    }
  }

  if (configured) {
    if (zx_status_t status = function_.ConfigEp(&descriptors_.intr_ep, NULL); status != ZX_OK) {
      zxlogf(ERROR, "[bug] ConfigEp(): %s", zx_status_get_string(status));
      return status;
    }
    speed_ = speed;
    configured_ = configured;
    cdc_send_notifications();
  } else {
    function_.DisableEp(bulk_out_addr_);
    function_.DisableEp(bulk_in_addr_);
    function_.DisableEp(intr_addr_);
    speed_ = USB_SPEED_UNDEFINED;
    configured_ = configured;
  }

  return ZX_OK;
}

zx_status_t UsbCdc::UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting) {
  TRACE_DURATION("cdc_eth", __func__, "interface", interface, "alt_setting", alt_setting);

  if (interface != descriptors_.cdc_intf_0.b_interface_number || alt_setting > 1) {
    return ZX_ERR_INVALID_ARGS;
  }

  // TODO(voydanoff) fullspeed and superspeed support
  if (alt_setting) {
    for (const auto* ep : {&descriptors_.bulk_out_ep, &descriptors_.bulk_in_ep}) {
      if (zx_status_t status = function_.ConfigEp(ep, nullptr); status != ZX_OK) {
        zxlogf(ERROR, "[bug] ConfigEp(): %s", zx_status_get_string(status));
        return status;
      }
    }
  } else {
    for (const uint8_t ep : {bulk_out_addr_, bulk_in_addr_}) {
      if (zx_status_t status = function_.DisableEp(ep); status != ZX_OK) {
        zxlogf(ERROR, "[bug] DisableEp(): %s", zx_status_get_string(status));
        return status;
      }
    }
  }

  bool online;
  if (alt_setting) {
    online = true;

    // queue our OUT reqs
    std::lock_guard<std::mutex> rx(rx_mutex_);
    while (!bulk_out_ep_.RequestsEmpty()) {
      std::optional<usb::FidlRequest> req = bulk_out_ep_.GetRequest();
      ZX_ASSERT(req.has_value());  // A given from the loop.
      req->reset_buffers(bulk_out_ep_.GetMappedLocked());

      if (zx_status_t status = req->CacheFlushInvalidate(bulk_out_ep_.GetMappedLocked());
          status != ZX_OK) {
        zxlogf(ERROR, "[bug] CacheFlushInvalidate(): %s", zx_status_get_string(status));
        return status;
      }
      usb_request_queue(std::move(req.value()), bulk_out_ep_);
    }
  } else {
    online = false;
  }

  {
    std::lock_guard<std::mutex> ethernet(ethernet_mutex_);
    online_ = online;
    if (ethernet_ifc_.is_valid()) {
      ethernet_ifc_.Status(online ? ETHERNET_STATUS_ONLINE : 0);
    }
  }

  // send status notifications on interrupt endpoint
  cdc_send_notifications();

  return ZX_OK;
}

void UsbCdc::DdkInit(ddk::InitTxn txn) {
  list_initialize(&tx_pending_infos_);

  auto status = function_.AllocInterface(&descriptors_.comm_intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] AllocInterface(comm_intf): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }
  status = function_.AllocInterface(&descriptors_.cdc_intf_0.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] AllocInterface(data_intf): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }
  descriptors_.cdc_intf_1.b_interface_number = descriptors_.cdc_intf_0.b_interface_number;
  descriptors_.cdc_union.bControlInterface = descriptors_.comm_intf.b_interface_number;
  descriptors_.cdc_union.bSubordinateInterface = descriptors_.cdc_intf_0.b_interface_number;

  status = function_.AllocEp(USB_DIR_OUT, &bulk_out_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] AllocEp(bulk_out): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }
  status = function_.AllocEp(USB_DIR_IN, &bulk_in_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] AllocEp(bulk_in): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }
  status = function_.AllocEp(USB_DIR_IN, &intr_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] AllocEp(intr): %s", zx_status_get_string(status));
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
    zxlogf(ERROR, "[bug] fdf::SynchronizedDispatcher::Create(): %s", dispatcher.status_string());
    txn.Reply(dispatcher.error_value());
    return;
  }
  dispatcher_ = std::move(dispatcher.value());

  auto result = DdkConnectFidlProtocol<ffunction::UsbFunctionService::Device>(parent());
  if (result.is_error()) {
    zxlogf(ERROR, "could not connect to UsbFunctionService: %s", result.status_string());
    txn.Reply(result.error_value());
    return;
  }

  // allocate bulk out usb requests
  status = bulk_out_ep_.Init(bulk_out_addr_, result.value(), dispatcher_.async_dispatcher());
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] bulk_out_ep_.Init(): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }

  size_t actual =
      bulk_out_ep_.AddRequests(BULK_RX_COUNT, BULK_REQ_SIZE, frequest::Buffer::Tag::kVmoId);
  if (actual != BULK_RX_COUNT) {
    zxlogf(ERROR, "[bug] bulk_out_ep_.AddRequests(): want %d, got %zu", BULK_RX_COUNT, actual);
    txn.Reply(ZX_ERR_INTERNAL);
    return;
  }

  // allocate bulk in usb requests
  status = bulk_in_ep_.Init(bulk_in_addr_, result.value(), dispatcher_.async_dispatcher());
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] bulk_in_ep_.Init(): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }

  actual = bulk_in_ep_.AddRequests(BULK_TX_COUNT, BULK_REQ_SIZE, frequest::Buffer::Tag::kVmoId);
  if (actual != BULK_TX_COUNT) {
    zxlogf(ERROR, "[bug] bulk_in_ep_.AddRequests(): want %d, got %zu", BULK_TX_COUNT, actual);
    txn.Reply(ZX_ERR_INTERNAL);
    return;
  }

  // allocate interrupt requests
  status = intr_ep_.Init(intr_addr_, result.value(), dispatcher_.async_dispatcher());
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] intr_ep_.Init(): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }

  actual = intr_ep_.AddRequests(INTR_COUNT, BULK_REQ_SIZE, frequest::Buffer::Tag::kVmoId);
  if (actual != INTR_COUNT) {
    zxlogf(ERROR, "[bug] intr_ep_.AddRequests(): want %d, got %zu", INTR_COUNT, actual);
    txn.Reply(ZX_ERR_INTERNAL);
    return;
  }

  status = function_.SetInterface(this, &usb_function_interface_protocol_ops_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] function_.SetInterface(): %s", zx_status_get_string(status));
    txn.Reply(status);
    return;
  }

  txn.Reply(ZX_OK);
}

void UsbCdc::DdkUnbind(ddk::UnbindTxn txn) {
  unbound_ = true;
  {
    std::lock_guard<std::mutex> l(tx_mutex_);
    txn_info_t* txn;
    while ((txn = list_remove_head_type(tx_pending_infos(), txn_info_t, node)) != NULL) {
      complete_txn(txn, ZX_ERR_PEER_CLOSED);
    }
  }

  txn.Reply();
}

void UsbCdc::DdkRelease() {
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
      std::lock_guard<std::mutex> l(tx_mutex_);
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
  auto cdc = std::make_unique<UsbCdc>(parent);
  if (!cdc) {
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = cdc->DdkAdd("cdc-eth-function");

  // Either the DDK now owns this reference, or (in the case of failure) it will be freed in the
  // block below. In either case, we want to avoid the unique_ptr destructing the allocated
  // UsbCdc instance.
  [[maybe_unused]] auto released = cdc.release();

  if (status != ZX_OK) {
    zxlogf(ERROR, "adding device fails: %s", zx_status_get_string(status));
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
