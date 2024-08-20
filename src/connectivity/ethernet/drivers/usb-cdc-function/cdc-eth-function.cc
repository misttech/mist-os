// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/ethernet/drivers/usb-cdc-function/cdc-eth-function.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/trace/event.h>

#include <fbl/auto_lock.h>
#include <usb/usb-request.h>

namespace usb_cdc_function {

typedef struct txn_info {
  ethernet_netbuf_t netbuf;
  ethernet_impl_queue_tx_callback completion_cb;
  void* cookie;
  list_node_t node;
} txn_info_t;

static void complete_txn(txn_info_t* txn, zx_status_t status) {
  txn->completion_cb(txn->cookie, status, &txn->netbuf);
}

zx_status_t UsbCdc::instrumented_request_alloc(usb_request_t** out, uint64_t data_size,
                                               uint8_t ep_address, size_t req_size) {
  allocated_requests_count_++;
  return usb_request_alloc(out, data_size, ep_address, req_size);
}

void UsbCdc::instrumented_request_release(usb_request_t* req) {
  usb_request_release(req);
  allocated_requests_count_--;
  if (suspend_txn_.has_value() && (allocated_requests_count_.load() == 0)) {
    sync_completion_signal(&requests_freed_completion_);
  }
}

zx_status_t UsbCdc::insert_usb_request(list_node_t* list, usb_request_t* req,
                                       size_t parent_req_size, bool tail) {
  if (suspend_txn_.has_value()) {
    instrumented_request_release(req);
    return ZX_OK;
  }
  if (tail) {
    return usb_req_list_add_tail(list, req, parent_req_size);
  } else {
    return usb_req_list_add_head(list, req, parent_req_size);
  }
}

void UsbCdc::usb_request_callback(void* ctx, usb_request_t* req) {
  auto cdc = reinterpret_cast<UsbCdc*>(ctx);
  if (cdc->suspend_txn_.has_value()) {
    cdc->instrumented_request_release(req);
    return;
  }
  // Invoke the real completion if not shutting down.
  if (!cdc->unbound_) {
    usb_request_complete_callback_t completion;
    memcpy(&completion, reinterpret_cast<unsigned char*>(req) + cdc->usb_request_offset_,
           sizeof(completion));
    completion.callback(completion.ctx, req);
  }
  {
    fbl::AutoLock _(&cdc->pending_request_lock_);
    int value = --cdc->pending_request_count_;
    if (value == 0) {
      cnd_signal(&cdc->pending_requests_completed_);
    }
  }
}

void UsbCdc::usb_request_queue(usb_request_t* req,
                               const usb_request_complete_callback_t* completion) {
  if (suspend_txn_.has_value()) {
    instrumented_request_release(req);
    return;
  }
  mtx_lock(&pending_request_lock_);
  if (unbound_) {
    mtx_unlock(&pending_request_lock_);
    return;
  }
  pending_request_count_++;
  mtx_unlock(&pending_request_lock_);
  usb_request_complete_callback_t internal_completion;
  internal_completion.callback = usb_request_callback;
  internal_completion.ctx = this;
  memcpy(reinterpret_cast<unsigned char*>(req) + usb_request_offset_, completion,
         sizeof(*completion));
  function_.RequestQueue(req, &internal_completion);
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

  mtx_lock(&tx_mutex_);
  mtx_lock(&ethernet_mutex_);
  ethernet_ifc_.clear();
  mtx_unlock(&ethernet_mutex_);
  mtx_unlock(&tx_mutex_);
}

zx_status_t UsbCdc::EthernetImplStart(const ethernet_ifc_protocol_t* ifc) {
  zxlogf(DEBUG, "%s:", __func__);

  zx_status_t status = ZX_OK;
  if (unbound_) {
    return ZX_ERR_BAD_STATE;
  }
  mtx_lock(&ethernet_mutex_);
  if (ethernet_ifc_.is_valid()) {
    status = ZX_ERR_ALREADY_BOUND;
  } else {
    ethernet_ifc_ = ddk::EthernetIfcProtocolClient(ifc);
    ethernet_ifc_.Status(online_ ? ETHERNET_STATUS_ONLINE : 0);
  }
  mtx_unlock(&ethernet_mutex_);

  return status;
}

zx_status_t UsbCdc::cdc_send_locked(ethernet_netbuf_t* netbuf) {
  {
    fbl::AutoLock _(&ethernet_mutex_);
    if (!ethernet_ifc_.is_valid()) {
      return ZX_ERR_BAD_STATE;
    }
  }

  const auto* byte_data = static_cast<const uint8_t*>(netbuf->data_buffer);
  size_t length = netbuf->data_size;

  // Make sure that we can get all of the tx buffers we need to use
  usb_request_t* tx_req = usb_req_list_remove_head(bulk_in_reqs(), parent_req_size_);
  if (tx_req == NULL) {
    return ZX_ERR_SHOULD_WAIT;
  }

  // Send data
  tx_req->header.length = length;
  ssize_t bytes_copied = usb_request_copy_to(tx_req, byte_data, tx_req->header.length, 0);
  if (bytes_copied < 0) {
    zxlogf(SERIAL, "%s: failed to copy data into send req (error %zd)", __func__, bytes_copied);
    zx_status_t status = insert_usb_request(bulk_in_reqs(), tx_req, parent_req_size_);
    ZX_DEBUG_ASSERT(status == ZX_OK);
    return ZX_ERR_INTERNAL;
  }

  usb_request_complete_callback_t complete = {
      .callback = cdc_tx_complete,
      .ctx = this,
  };
  usb_request_queue(tx_req, &complete);

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
    fbl::AutoLock _(&ethernet_mutex_);
    if (!online_ || length > ETH_MTU || length == 0 || unbound_) {
      complete_txn(txn, ZX_ERR_INVALID_ARGS);
      return;
    }
  }

  zxlogf(SERIAL, "%s: sending %zu bytes", __func__, length);

  mtx_lock(&tx_mutex_);
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

  mtx_unlock(&tx_mutex_);
  if (status != ZX_ERR_SHOULD_WAIT) {
    complete_txn(txn, status);
  }
}

zx_status_t UsbCdc::EthernetImplSetParam(uint32_t param, int32_t value, const uint8_t* data_buffer,
                                         size_t data_size) {
  return ZX_ERR_NOT_SUPPORTED;
}

void UsbCdc::cdc_intr_complete(void* ctx, usb_request_t* req) {
  zxlogf(SERIAL, "%s %d %ld", __func__, req->response.status, req->response.actual);

  auto cdc = reinterpret_cast<UsbCdc*>(ctx);
  mtx_lock(&cdc->intr_mutex_);
  if (cdc->suspend_txn_.has_value()) {
    cdc->instrumented_request_release(req);
  } else {
    zx_status_t status = cdc->insert_usb_request(cdc->intr_reqs(), req, cdc->parent_req_size_);
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }
  mtx_unlock(&cdc->intr_mutex_);
}

void UsbCdc::cdc_send_notifications() {
  usb_request_t* req;

  fbl::AutoLock _(&ethernet_mutex_);

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
  mtx_lock(&intr_mutex_);
  req = usb_req_list_remove_head(intr_reqs(), parent_req_size_);
  mtx_unlock(&intr_mutex_);
  if (!req) {
    zxlogf(ERROR, "%s: no interrupt request available", __func__);
    return;
  }

  auto result = usb_request_copy_to(req, &network_notification, sizeof(network_notification), 0);
  ZX_ASSERT(result == sizeof(network_notification));
  req->header.length = sizeof(network_notification);

  usb_request_complete_callback_t complete = {
      .callback = cdc_intr_complete,
      .ctx = this,
  };
  usb_request_queue(req, &complete);
  mtx_lock(&intr_mutex_);
  req = usb_req_list_remove_head(intr_reqs(), parent_req_size_);
  mtx_unlock(&intr_mutex_);
  if (!req) {
    zxlogf(ERROR, "%s: no interrupt request available", __func__);
    return;
  }

  result = usb_request_copy_to(req, &speed_notification, sizeof(speed_notification), 0);
  ZX_ASSERT(result == sizeof(speed_notification));
  req->header.length = sizeof(speed_notification);

  usb_request_queue(req, &complete);
}

void UsbCdc::cdc_rx_complete(void* ctx, usb_request_t* req) {
  zxlogf(SERIAL, "%s %d %ld", __func__, req->response.status, req->response.actual);

  auto cdc = reinterpret_cast<UsbCdc*>(ctx);
  if (req->response.status == ZX_ERR_IO_NOT_PRESENT) {
    mtx_lock(&cdc->rx_mutex_);
    zx_status_t status =
        cdc->insert_usb_request(cdc->bulk_out_reqs(), req, cdc->parent_req_size_, false);
    ZX_DEBUG_ASSERT(status == ZX_OK);
    mtx_unlock(&cdc->rx_mutex_);
    return;
  }
  if (req->response.status != ZX_OK) {
    zxlogf(ERROR, "%s: usb_read_complete called with status %d", __func__, req->response.status);
  }

  if (req->response.status == ZX_OK) {
    mtx_lock(&cdc->ethernet_mutex_);
    if (cdc->ethernet_ifc_.is_valid()) {
      void* data = NULL;
      usb_request_mmap(req, &data);
      cdc->ethernet_ifc_.Recv(reinterpret_cast<uint8_t*>(data), req->response.actual, 0);
    }
    mtx_unlock(&cdc->ethernet_mutex_);
  }

  usb_request_complete_callback_t complete = {
      .callback = cdc_rx_complete,
      .ctx = ctx,
  };
  cdc->usb_request_queue(req, &complete);
}

void UsbCdc::cdc_tx_complete(void* ctx, usb_request_t* req) {
  zxlogf(SERIAL, "%s %d %ld", __func__, req->response.status, req->response.actual);

  auto cdc = reinterpret_cast<UsbCdc*>(ctx);
  if (cdc->unbound_) {
    return;
  }
  mtx_lock(&cdc->tx_mutex_);
  {
    if (cdc->suspend_txn_.has_value()) {
      mtx_unlock(&cdc->tx_mutex_);
      cdc->instrumented_request_release(req);
      return;
    }
    zx_status_t status = cdc->insert_usb_request(cdc->bulk_in_reqs(), req, cdc->parent_req_size_);
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }

  bool additional_tx_queued = false;
  txn_info_t* txn;
  zx_status_t send_status = ZX_OK;

  // Do not queue requests if status is ZX_ERR_IO_NOT_PRESENT, as the underlying connection could be
  // disconnected or USB_RESET is being processed. Calling cdc_send_locked in such scenario will
  // deadlock and crash the driver (see https://fxbug.dev/42174506).
  if (req->response.status != ZX_ERR_IO_NOT_PRESENT) {
    if ((txn = list_peek_head_type(cdc->tx_pending_infos(), txn_info_t, node))) {
      if ((send_status = cdc->cdc_send_locked(&txn->netbuf)) != ZX_ERR_SHOULD_WAIT) {
        list_remove_head(cdc->tx_pending_infos());
        additional_tx_queued = true;
      }
    }
  }
  mtx_unlock(&cdc->tx_mutex_);

  if (additional_tx_queued) {
    mtx_lock(&cdc->ethernet_mutex_);
    complete_txn(txn, send_status);
    mtx_unlock(&cdc->ethernet_mutex_);
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
  mtx_lock(&ethernet_mutex_);
  zxlogf(DEBUG, "%s: after crit_enter", __func__);
  online_ = false;
  if (ethernet_ifc_.is_valid()) {
    ethernet_ifc_.Status(0);
  }
  mtx_unlock(&ethernet_mutex_);
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
    mtx_lock(&rx_mutex_);
    usb_request_t* req;
    while ((req = usb_req_list_remove_head(bulk_out_reqs(), parent_req_size_)) != NULL) {
      usb_request_complete_callback_t complete = {
          .callback = cdc_rx_complete,
          .ctx = this,
      };
      usb_request_queue(req, &complete);
    }
    mtx_unlock(&rx_mutex_);
  }

  mtx_lock(&ethernet_mutex_);
  online_ = online;
  if (ethernet_ifc_.is_valid()) {
    ethernet_ifc_.Status(online ? ETHERNET_STATUS_ONLINE : 0);
  }
  mtx_unlock(&ethernet_mutex_);

  // send status notifications on interrupt endpoint
  cdc_send_notifications();

  return status;
}

void UsbCdc::DdkUnbind(ddk::UnbindTxn txn) {
  zxlogf(DEBUG, "%s", __func__);
  unbound_ = true;
  {
    fbl::AutoLock l(&pending_request_lock_);
    while (pending_request_count_) {
      cnd_wait(&pending_requests_completed_, &pending_request_lock_);
    }
  }
  {
    fbl::AutoLock l(&tx_mutex_);
    txn_info_t* txn;
    while ((txn = list_remove_head_type(tx_pending_infos(), txn_info_t, node)) != NULL) {
      complete_txn(txn, ZX_ERR_PEER_CLOSED);
    }
  }

  txn.Reply();
}

void UsbCdc::DdkRelease() {
  zxlogf(DEBUG, "%s", __func__);
  usb_request_t* req;
  {
    fbl::AutoLock _(&rx_mutex_);
    while ((req = usb_req_list_remove_head(bulk_out_reqs(), parent_req_size_)) != NULL) {
      instrumented_request_release(req);
    }
  }
  {
    fbl::AutoLock _(&tx_mutex_);
    while ((req = usb_req_list_remove_head(bulk_in_reqs(), parent_req_size_)) != NULL) {
      instrumented_request_release(req);
    }
  }
  {
    fbl::AutoLock _(&intr_mutex_);
    while ((req = usb_req_list_remove_head(intr_reqs(), parent_req_size_)) != NULL) {
      instrumented_request_release(req);
    }
  }
  mtx_destroy(&ethernet_mutex_);
  mtx_destroy(&tx_mutex_);
  mtx_destroy(&rx_mutex_);
  mtx_destroy(&intr_mutex_);
  if (suspend_thread_.has_value()) {
    suspend_thread_->join();
  }
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

    // Requests external to us should have been returned (or in the process of being returned)
    // at this point. Acquire all the locks to ensure that nothing touches any of
    // our request lists, and complete all requests. If an ongoing transaction
    // tries to add to one of these lists, since suspending was set to true,
    // the request will be free'd instead.
    list_node_t tmp_bulk_out_reqs;
    list_node_t tmp_bulk_in_reqs;
    list_node_t tmp_intr_reqs;
    {
      mtx_lock(&intr_mutex_);
      list_move(intr_reqs(), &tmp_intr_reqs);
      mtx_unlock(&intr_mutex_);
      mtx_lock(&rx_mutex_);
      list_move(bulk_out_reqs(), &tmp_bulk_out_reqs);
      mtx_unlock(&rx_mutex_);
      mtx_lock(&tx_mutex_);
      list_move(bulk_in_reqs(), &tmp_bulk_in_reqs);
      mtx_unlock(&tx_mutex_);
    }
    usb_request_t* req;
    while ((req = usb_req_list_remove_head(&tmp_bulk_out_reqs, parent_req_size_)) != NULL) {
      instrumented_request_release(req);
    }
    while ((req = usb_req_list_remove_head(&tmp_bulk_in_reqs, parent_req_size_)) != NULL) {
      instrumented_request_release(req);
    }
    while ((req = usb_req_list_remove_head(&tmp_intr_reqs, parent_req_size_)) != NULL) {
      instrumented_request_release(req);
    }

    // Wait for all the requests in the pipeline to asynchronously fail.
    // Either the completion routine or the submitter should free the requests.
    // It shouldn't be possible to have any "stray" requests that aren't in-flight at this point,
    // so this is guaranteed to complete.
    sync_completion_wait(&requests_freed_completion_, ZX_TIME_INFINITE);
    list_node_t list;
    {
      fbl::AutoLock l(&tx_mutex_);
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

  cnd_init(&cdc->pending_requests_completed_);
  list_initialize(&cdc->bulk_out_reqs_);
  list_initialize(&cdc->bulk_in_reqs_);
  list_initialize(&cdc->intr_reqs_);
  list_initialize(&cdc->tx_pending_infos_);
  mtx_init(&cdc->ethernet_mutex_, mtx_plain);
  mtx_init(&cdc->tx_mutex_, mtx_plain);
  mtx_init(&cdc->rx_mutex_, mtx_plain);
  mtx_init(&cdc->intr_mutex_, mtx_plain);

  cdc->bulk_max_packet_ = BULK_MAX_PACKET;  // FIXME(voydanoff) USB 3.0 support
  cdc->parent_req_size_ = cdc->function_.GetRequestSize();
  uint64_t req_size =
      cdc->parent_req_size_ + sizeof(usb_req_internal_t) + sizeof(usb_request_complete_callback_t);
  cdc->usb_request_offset_ = cdc->parent_req_size_ + sizeof(usb_req_internal_t);
  auto status = cdc->function_.AllocInterface(&cdc->descriptors_.comm_intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: AllocInterface failed", __func__);
    cdc->DdkRelease();
    return status;
  }
  status = cdc->function_.AllocInterface(&cdc->descriptors_.cdc_intf_0.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: AllocInterface failed", __func__);
    cdc->DdkRelease();
    return status;
  }
  cdc->descriptors_.cdc_intf_1.b_interface_number = cdc->descriptors_.cdc_intf_0.b_interface_number;
  cdc->descriptors_.cdc_union.bControlInterface = cdc->descriptors_.comm_intf.b_interface_number;
  cdc->descriptors_.cdc_union.bSubordinateInterface =
      cdc->descriptors_.cdc_intf_0.b_interface_number;

  status = cdc->function_.AllocEp(USB_DIR_OUT, &cdc->bulk_out_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: AllocEp failed", __func__);
    cdc->DdkRelease();
    return status;
  }
  status = cdc->function_.AllocEp(USB_DIR_IN, &cdc->bulk_in_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: AllocEp failed", __func__);
    cdc->DdkRelease();
    return status;
  }
  status = cdc->function_.AllocEp(USB_DIR_IN, &cdc->intr_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: AllocEp failed", __func__);
    cdc->DdkRelease();
    return status;
  }

  cdc->descriptors_.bulk_out_ep.b_endpoint_address = cdc->bulk_out_addr_;
  cdc->descriptors_.bulk_in_ep.b_endpoint_address = cdc->bulk_in_addr_;
  cdc->descriptors_.intr_ep.b_endpoint_address = cdc->intr_addr_;

  status = cdc->cdc_generate_mac_address();
  if (status != ZX_OK) {
    cdc->DdkRelease();
    return status;
  }

  // allocate bulk out usb requests
  usb_request_t* req;
  for (int i = 0; i < BULK_TX_COUNT; i++) {
    status = cdc->instrumented_request_alloc(&req, BULK_REQ_SIZE, cdc->bulk_out_addr_, req_size);
    if (status != ZX_OK) {
      cdc->DdkRelease();
      return status;
    }
    status = usb_req_list_add_head(&cdc->bulk_out_reqs_, req, cdc->parent_req_size_);
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }
  // allocate bulk in usb requests
  for (int i = 0; i < BULK_RX_COUNT; i++) {
    status = cdc->instrumented_request_alloc(&req, BULK_REQ_SIZE, cdc->bulk_in_addr_, req_size);
    if (status != ZX_OK) {
      cdc->DdkRelease();
      return status;
    }

    // As per the CDC-ECM spec, we need to send a zero-length packet to signify the end of
    // transmission when the endpoint max packet size is a factor of the total transmission size
    req->header.send_zlp = true;

    status = usb_req_list_add_head(&cdc->bulk_in_reqs_, req, cdc->parent_req_size_);
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }

  // allocate interrupt requests
  for (int i = 0; i < INTR_COUNT; i++) {
    status = cdc->instrumented_request_alloc(&req, INTR_MAX_PACKET, cdc->intr_addr_, req_size);
    if (status != ZX_OK) {
      cdc->DdkRelease();
      return status;
    }

    status = usb_req_list_add_head(&cdc->intr_reqs_, req, cdc->parent_req_size_);
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }

  status = cdc->DdkAdd("cdc-eth-function");
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not add UsbCdc %s.", zx_status_get_string(status));
    cdc->DdkRelease();
    return status;
  }
  cdc->function_.SetInterface(cdc.get(), &cdc->usb_function_interface_protocol_ops_);
  {
    // The DDK now owns this reference.
    [[maybe_unused]] auto released = cdc.release();
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
