// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-bus.h"

#include <assert.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/sync/cpp/completion.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <variant>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/test/platform/cpp/bind.h>
#include <ddktl/fidl.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <usb/usb.h>

namespace usb_virtual_bus {

namespace {

inline usb_setup_t ToBanjo(fuchsia_hardware_usb_descriptor::UsbSetup setup) {
  return usb_setup_t{
      .bm_request_type = setup.bm_request_type(),
      .b_request = setup.b_request(),
      .w_value = setup.w_value(),
      .w_index = setup.w_index(),
      .w_length = setup.w_length(),
  };
}

}  // namespace

static constexpr uint8_t IN_EP_START = 17;

#define DEVICE_SLOT_ID 0
#define DEVICE_HUB_ID 0
#define DEVICE_SPEED USB_SPEED_HIGH

zx_status_t UsbVirtualBus::Create(zx_device_t* parent) {
  fbl::AllocChecker ac;
  auto bus = fbl::make_unique_checked<UsbVirtualBus>(
      &ac, parent, fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto status = bus->Init();
  if (status != ZX_OK) {
    return status;
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* dummy = bus.release();
  return ZX_OK;
}

zx_status_t UsbVirtualBus::CreateDevice() {
  fbl::AllocChecker ac;
  device_ = fbl::make_unique_checked<UsbVirtualDevice>(&ac, zxdev(), this);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    device_ = nullptr;
    return endpoints.status_value();
  }
  zx::result result = outgoing_.AddService<fuchsia_hardware_usb_dci::UsbDciService>(
      fuchsia_hardware_usb_dci::UsbDciService::InstanceHandler({
          .device =
              dci_bindings_.CreateHandler(device_.get(), dispatcher_, fidl::kIgnoreBindingClosure),
      }));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service");
    device_ = nullptr;
    return result.status_value();
  }
  result = outgoing_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to service the outgoing directory");
    device_ = nullptr;
    return result.status_value();
  }

  const zx_device_str_prop_t props[] = {
    ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_VID,
                         bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST),
    ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_PID,
                         bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_USB),
    ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_DID,
                         bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_VIRTUAL_DEVICE),
  };

  std::array offers = {
      fuchsia_hardware_usb_dci::UsbDciService::Name,
  };
  auto status = device_->DdkAdd(ddk::DeviceAddArgs("usb-virtual-device")
                                    .set_str_props(props)
                                    .set_fidl_service_offers(offers)
                                    .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    device_ = nullptr;
    return status;
  }

  return ZX_OK;
}

zx_status_t UsbVirtualBus::CreateHost() {
  fbl::AllocChecker ac;
  host_ = fbl::make_unique_checked<UsbVirtualHost>(&ac, zxdev(), this);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    host_ = nullptr;
    return endpoints.status_value();
  }
  zx::result result = outgoing_.AddService<fuchsia_hardware_usb_hci::UsbHciService>(
      fuchsia_hardware_usb_hci::UsbHciService::InstanceHandler({
          .device =
              hci_bindings_.CreateHandler(host_.get(), dispatcher_, fidl::kIgnoreBindingClosure),
      }));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service");
    host_ = nullptr;
    return result.status_value();
  }
  result = outgoing_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to service the outgoing directory");
    host_ = nullptr;
    return result.status_value();
  }

  const zx_device_str_prop_t props[] = {
    ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_VID,
                         bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST),
    ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_PID,
                         bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_USB),
    ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_DID,
                         bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_VIRTUAL_BUS),
  };

  std::array offers = {
      fuchsia_hardware_usb_hci::UsbHciService::Name,
  };
  auto status = host_->DdkAdd(ddk::DeviceAddArgs("usb-virtual-host")
                                  .set_str_props(props)
                                  .set_fidl_service_offers(offers)
                                  .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    host_ = nullptr;
    return status;
  }

  return ZX_OK;
}

zx_status_t UsbVirtualBus::Init() { return DdkAdd("usb-virtual-bus", DEVICE_ADD_NON_BINDABLE); }

void UsbVirtualBus::DdkInit(ddk::InitTxn txn) {
  auto result = fdf::SynchronizedDispatcher::Create(
      {}, "usb-virtual-bus-device-thread",
      [this](fdf_dispatcher_t*) { device_dispatcher_shutdown_wait_.Signal(); });
  if (!result.is_ok()) {
    return txn.Reply(result.status_value());
  }
  device_dispatcher_ = std::move(*result);
  return txn.Reply(ZX_OK);
}

void UsbVirtualBus::ProcessRequests() {
  struct complete_request_t {
    complete_request_t(UsbVirtualEp& ep, RequestVariant request, zx_status_t status, size_t actual)
        : ep(ep), request(std::move(request)), status(status), actual(actual) {}

    UsbVirtualEp& ep;
    RequestVariant request;
    zx_status_t status;
    size_t actual;
  };
  std::queue<complete_request_t> complete_reqs_pending;

  auto add_complete_reqs = [&complete_reqs_pending](UsbVirtualEp& ep, RequestVariant request,
                                                    zx_status_t status, size_t actual) {
    complete_reqs_pending.emplace(ep, std::move(request), status, actual);
  };
  auto process_complete_reqs = [&complete_reqs_pending]() {
    while (!complete_reqs_pending.empty()) {
      auto& complete = complete_reqs_pending.front();
      complete.ep.RequestComplete(complete.status, complete.actual, std::move(complete.request));
      complete_reqs_pending.pop();
    }
  };

  {
    fbl::AutoLock al(&device_lock_);
    bool has_work = true;
    while (has_work) {
      has_work = false;
      if (unbinding_) {
        for (auto& ep : eps_) {
          for (auto req = ep.device_reqs.pop(); req; req = ep.device_reqs.pop()) {
            add_complete_reqs(ep, std::move(req.value()), ZX_ERR_IO_NOT_PRESENT, 0);
          }
        }
        // We need to wait for all control requests to complete before completing the unbind.
        if (num_pending_control_reqs_ > 0) {
          complete_unbind_signal_.Wait(&device_lock_);
        }

        al.release();
        process_complete_reqs();

        // At this point, all pending control requests have been completed,
        // and any queued requests wil be immediately completed with an error.
        ZX_DEBUG_ASSERT(unbind_txn_.has_value());
        unbind_txn_->Reply();
        return;
      }
      // Data transfer between device/host (everything except ep 0)
      for (uint8_t i = 0; i < USB_MAX_EPS; i++) {
        auto& ep = eps_[i];
        while (!ep.host_reqs.empty() && !ep.device_reqs.is_empty()) {
          has_work = true;
          auto device_req = std::move(ep.device_reqs.pop().value());
          auto req = std::move(ep.host_reqs.front());
          ep.host_reqs.pop();
          size_t length = std::min(std::holds_alternative<usb::FidlRequest>(req)
                                       ? std::get<usb::FidlRequest>(req).length()
                                       : std::get<Request>(req).request()->header.length,
                                   device_req.request()->header.length);
          void* device_buffer;
          auto status = device_req.Mmap(&device_buffer);
          if (status != ZX_OK) {
            zxlogf(ERROR, "%s: usb_request_mmap failed: %d", __func__, status);
            add_complete_reqs(ep, std::move(req), status, 0);
            add_complete_reqs(ep, std::move(device_req), status, 0);
            continue;
          }

          if (i < IN_EP_START) {
            if (std::holds_alternative<usb::FidlRequest>(req)) {
              std::vector<size_t> result = std::get<usb::FidlRequest>(req).CopyFrom(
                  0, device_buffer, length,
                  fit::bind_member(host_->ep(i), &UsbVirtualHost::UsbEpServer::GetMapped));
              ZX_ASSERT(result.size() == 1);
              ZX_ASSERT(result[0] == length);
            } else {
              size_t result = std::get<Request>(req).CopyFrom(device_buffer, length, 0);
              ZX_ASSERT(result == length);
            }
          } else {
            if (std::holds_alternative<usb::FidlRequest>(req)) {
              std::vector<size_t> result = std::get<usb::FidlRequest>(req).CopyTo(
                  0, device_buffer, length,
                  fit::bind_member(host_->ep(i), &UsbVirtualHost::UsbEpServer::GetMapped));
              ZX_ASSERT(result.size() == 1);
              ZX_ASSERT(result[0] == length);
            } else {
              size_t result = std::get<Request>(req).CopyTo(device_buffer, length, 0);
              ZX_ASSERT(result == length);
            }
          }
          add_complete_reqs(ep, std::move(req), ZX_OK, length);
          add_complete_reqs(ep, std::move(device_req), ZX_OK, length);
        }
      }
    }
  }
  process_complete_reqs();
}

void UsbVirtualBus::HandleControl(RequestVariant req) {
  const usb_setup_t& setup =
      std::holds_alternative<usb::FidlRequest>(req)
          ? ToBanjo(*std::get<usb::FidlRequest>(req)->information()->control()->setup())
          : std::get<Request>(req).request()->setup;
  zx_status_t status;
  size_t length = le16toh(setup.w_length);
  size_t actual = 0;

  zxlogf(DEBUG, "%s type: 0x%02X req: %d value: %d index: %d length: %zu", __func__,
         setup.bm_request_type, setup.b_request, le16toh(setup.w_value), le16toh(setup.w_index),
         length);

  if (dci_intf_.is_valid()) {
    void* buffer = nullptr;

    if (length > 0) {
      if (std::holds_alternative<usb::FidlRequest>(req)) {
        auto& request_buffer = (*std::get<usb::FidlRequest>(req)->data())[0].buffer();
        const auto& buffer_type = request_buffer->Which();
        switch (buffer_type) {
          case fuchsia_hardware_usb_request::Buffer::Tag::kVmoId:
            buffer = reinterpret_cast<void*>(
                host_->ep(0)->registered_vmo(request_buffer->vmo_id().value()).addr);
            break;
          case fuchsia_hardware_usb_request::Buffer::Tag::kData:
            buffer = request_buffer->data()->data();
            break;
          default:
            zxlogf(ERROR, "%s: Unknown buffer type %u", __func__,
                   static_cast<uint32_t>(buffer_type));
            return;
        }
      } else {
        auto status = std::get<Request>(req).Mmap(&buffer);
        if (status != ZX_OK) {
          zxlogf(ERROR, "%s: usb_request_mmap failed: %d", __func__, status);
          eps_[0].RequestComplete(status, 0, std::move(req));
          return;
        }
      }
    }

    if ((setup.bm_request_type & USB_ENDPOINT_DIR_MASK) == USB_ENDPOINT_IN) {
      status = dci_intf_.Control(&setup, nullptr, 0, reinterpret_cast<uint8_t*>(buffer), length,
                                 &actual);
    } else {
      status = dci_intf_.Control(&setup, reinterpret_cast<uint8_t*>(buffer), length, nullptr, 0,
                                 nullptr);
    }
  } else {
    status = ZX_ERR_UNAVAILABLE;
  }

  {
    fbl::AutoLock device_lock(&device_lock_);
    num_pending_control_reqs_--;
    if (unbinding_ && num_pending_control_reqs_ == 0) {
      // The worker thread is waiting for the control request to complete.
      complete_unbind_signal_.Signal();
    }
  }

  eps_[0].RequestComplete(status, actual, std::move(req));
}

void UsbVirtualBus::SetConnected(bool connected) {
  bool was_connected = connected;
  {
    fbl::AutoLock lock(&connection_lock_);
    std::swap(connected_, was_connected);
  }
  if (connected && !was_connected) {
    if (bus_intf_.is_valid()) {
      bus_intf_.AddDevice(DEVICE_SLOT_ID, DEVICE_HUB_ID, DEVICE_SPEED);
    }
    if (dci_intf_.is_valid()) {
      dci_intf_.SetConnected(true);
    }
  } else if (!connected && was_connected) {
    for (auto& ep : eps_) {
      std::queue<RequestVariant> queue;
      {
        fbl::AutoLock l(&lock_);
        fbl::AutoLock l2(&device_lock_);
        queue = std::move(ep.host_reqs);
        for (auto req = ep.device_reqs.pop(); req; req = ep.device_reqs.pop()) {
          queue.push(std::move(*req));
        }
      }
      while (!queue.empty()) {
        ep.RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, std::move(queue.front()));
        queue.pop();
      }
    }
    if (bus_intf_.is_valid()) {
      bus_intf_.RemoveDevice(DEVICE_SLOT_ID);
    }
    if (dci_intf_.is_valid()) {
      dci_intf_.SetConnected(false);
    }
  }
}

zx_status_t UsbVirtualBus::SetStall(uint8_t ep_address, bool stall) {
  uint8_t index = EpAddressToIndex(ep_address);
  if (index >= USB_MAX_EPS) {
    return ZX_ERR_INVALID_ARGS;
  }

  UsbVirtualEp& ep = eps_[index];
  std::optional<RequestVariant> req = std::nullopt;
  {
    fbl::AutoLock lock(&lock_);

    ep.stalled = stall;

    if (stall) {
      req.emplace(std::move(ep.host_reqs.front()));
      ep.host_reqs.pop();
    }
  }

  if (req) {
    ep.RequestComplete(ZX_ERR_IO_REFUSED, 0, std::move(*req));
  }

  return ZX_OK;
}

void UsbVirtualBus::DdkUnbind(ddk::UnbindTxn txn) {
  {
    fbl::AutoLock lock(&lock_);
    fbl::AutoLock lock2(&device_lock_);
    unbinding_ = true;
    // The device thread will reply to the unbind txn when ready.
    unbind_txn_ = std::move(txn);
    process_requests_.Post(device_dispatcher_.async_dispatcher());
  }
  auto* host = host_.release();
  if (host) {
    host->DdkAsyncRemove();
  }
  auto* device = device_.release();
  if (device) {
    device->DdkAsyncRemove();
  }
}

void UsbVirtualBus::DdkChildPreRelease(void* child_ctx) {
  if (host_.get() == child_ctx) {
    [[maybe_unused]] auto* host = host_.release();
    return;
  }

  if (device_.get() == child_ctx) {
    [[maybe_unused]] auto* device = device_.release();
    return;
  }
}

void UsbVirtualBus::DdkRelease() {
  device_dispatcher_.ShutdownAsync();
  if (disconnect_completer_) {
    thrd_join(disconnect_thread_, nullptr);
  }
  if (disable_completer_) {
    thrd_join(disable_thread_, nullptr);
  }
  device_dispatcher_shutdown_wait_.Wait();
  delete this;
}

zx_status_t UsbVirtualBus::UsbDciCancelAll(uint8_t endpoint) {
  uint8_t index = EpAddressToIndex(endpoint);
  if (index == 0 || index >= USB_MAX_EPS) {
    return ZX_ERR_INVALID_ARGS;
  }
  RequestQueue q;
  {
    fbl::AutoLock l(&device_lock_);
    q = std::move(eps_[index].device_reqs);
  }
  for (auto req = q.pop(); req; req = q.pop()) {
    req->Complete(ZX_ERR_IO_NOT_PRESENT, 0);
  }
  return ZX_OK;
}

void UsbVirtualBus::UsbDciRequestQueue(usb_request_t* req,
                                       const usb_request_complete_callback_t* complete_cb) {
  Request request(req, *complete_cb, sizeof(usb_request_t));

  uint8_t index = EpAddressToIndex(request.request()->header.ep_address);
  if (index == 0 || index >= USB_MAX_EPS) {
    printf("%s: bad endpoint %u\n", __func__, request.request()->header.ep_address);
    request.Complete(ZX_ERR_INVALID_ARGS, 0);
    return;
  }
  // NOTE: Don't check if we're connected here, because the DCI interface
  // may come up before the virtual cable is connected.
  // The functions have no way of knowing if the cable is connected
  // so we need to allow them to queue up requeste here in case
  // we're in the bringup phase, and the request is queued before the cable is connected.
  // (otherwise requests will never be completed).
  // The same is not true for the host side, which is why these are different.

  fbl::AutoLock lock(&device_lock_);
  if (unbinding_) {
    lock.release();
    request.Complete(ZX_ERR_IO_REFUSED, 0);
    return;
  }
  eps_[index].device_reqs.push(std::move(request));

  process_requests_.Post(device_dispatcher_.async_dispatcher());
}

zx_status_t UsbVirtualBus::UsbDciSetInterface(const usb_dci_interface_protocol_t* dci_intf) {
  if (dci_intf) {
    dci_intf_ = ddk::UsbDciInterfaceProtocolClient(dci_intf);
  } else {
    dci_intf_.clear();
  }
  return ZX_OK;
}

zx_status_t UsbVirtualBus::UsbDciConfigEp(const usb_endpoint_descriptor_t* ep_desc,
                                          const usb_ss_ep_comp_descriptor_t* ss_comp_desc) {
  uint8_t index = EpAddressToIndex(ep_desc->b_endpoint_address);
  if (index >= USB_MAX_EPS) {
    return ZX_ERR_INVALID_ARGS;
  }

  UsbVirtualEp& ep = eps_[index];
  ep.max_packet_size = usb_ep_max_packet(ep_desc);
  return ZX_OK;
}

zx_status_t UsbVirtualBus::UsbDciDisableEp(uint8_t ep_address) { return ZX_OK; }

zx_status_t UsbVirtualBus::UsbDciEpSetStall(uint8_t ep_address) {
  return SetStall(ep_address, true);
}

zx_status_t UsbVirtualBus::UsbDciEpClearStall(uint8_t ep_address) {
  return SetStall(ep_address, false);
}

size_t UsbVirtualBus::UsbDciGetRequestSize() {
  return Request::RequestSize(Request::RequestSize(sizeof(usb_request_t)));
}

void UsbVirtualBus::UsbHciRequestQueue(usb_request_t* req,
                                       const usb_request_complete_callback_t* complete_cb) {
  Request request(req, *complete_cb, sizeof(usb_request_t));

  uint8_t index = EpAddressToIndex(request.request()->header.ep_address);
  if (index >= USB_MAX_EPS) {
    printf("usb_virtual_bus_host_queue bad endpoint %u\n", request.request()->header.ep_address);
    request.Complete(ZX_ERR_INVALID_ARGS, 0);
    return;
  }

  eps_[index].QueueRequest(std::move(request));
}

void UsbVirtualEp::QueueRequest(RequestVariant request) {
  bool connected;
  {
    fbl::AutoLock _(&bus_->connection_lock_);
    connected = bus_->connected_;
  }
  if (!connected) {
    RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, std::move(request));
    return;
  }

  fbl::AutoLock device_lock(&bus_->device_lock_);
  if (bus_->unbinding_) {
    device_lock.release();
    RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, std::move(request));
    return;
  }

  if (stalled) {
    device_lock.release();
    RequestComplete(ZX_ERR_IO_REFUSED, 0, std::move(request));
    return;
  }
  if (!is_control()) {
    host_reqs.push(std::move(request));
    bus_->process_requests_.Post(bus_->device_dispatcher_.async_dispatcher());
  } else {
    bus_->num_pending_control_reqs_++;
    // Control messages are a VERY special case.
    // They are synchronous; so we shouldn't dispatch them
    // to an I/O thread.
    // We can't hold a lock when responding to a control request
    device_lock.release();
    bus_->HandleControl(std::move(request));
  }
}

void UsbVirtualBus::UsbHciSetBusInterface(const usb_bus_interface_protocol_t* bus_intf) {
  if (bus_intf) {
    bus_intf_ = ddk::UsbBusInterfaceProtocolClient(bus_intf);

    bool connected;
    {
      fbl::AutoLock al(&connection_lock_);
      connected = connected_;
    }

    if (connected && bus_intf_.is_valid()) {
      bus_intf_.AddDevice(DEVICE_SLOT_ID, DEVICE_HUB_ID, DEVICE_SPEED);
    }
  } else {
    bus_intf_.clear();
  }
}

size_t UsbVirtualBus::UsbHciGetMaxDeviceCount() { return 1; }

zx_status_t UsbVirtualBus::UsbHciEnableEndpoint(uint32_t device_id,
                                                const usb_endpoint_descriptor_t* ep_desc,
                                                const usb_ss_ep_comp_descriptor_t* ss_com_desc,
                                                bool enable) {
  return ZX_OK;
}

uint64_t UsbVirtualBus::UsbHciGetCurrentFrame() { return 0; }

zx_status_t UsbVirtualBus::UsbHciConfigureHub(uint32_t device_id, usb_speed_t speed,
                                              const usb_hub_descriptor_t* desc, bool multi_tt) {
  return ZX_OK;
}

zx_status_t UsbVirtualBus::UsbHciHubDeviceAdded(uint32_t device_id, uint32_t port,
                                                usb_speed_t speed) {
  return ZX_OK;
}

zx_status_t UsbVirtualBus::UsbHciHubDeviceRemoved(uint32_t device_id, uint32_t port) {
  return ZX_OK;
}

zx_status_t UsbVirtualBus::UsbHciHubDeviceReset(uint32_t device_id, uint32_t port) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t UsbVirtualBus::UsbHciResetEndpoint(uint32_t device_id, uint8_t ep_address) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t UsbVirtualBus::UsbHciResetDevice(uint32_t hub_address, uint32_t device_id) {
  return ZX_ERR_NOT_SUPPORTED;
}

size_t UsbVirtualBus::UsbHciGetMaxTransferSize(uint32_t device_id, uint8_t ep_address) {
  return 65536;
}

zx_status_t UsbVirtualBus::UsbHciCancelAll(uint32_t device_id, uint8_t ep_address) {
  uint8_t index = EpAddressToIndex(ep_address);
  return eps_[index].CancelAll().status_value();
}

zx::result<> UsbVirtualEp::CancelAll() {
  std::queue<RequestVariant> q;
  {
    fbl::AutoLock lock(&bus_->device_lock_);
    q = std::move(host_reqs);
  }
  while (!q.empty()) {
    RequestComplete(ZX_ERR_IO, 0, std::move(q.front()));
    q.pop();
  }
  return zx::ok();
}

size_t UsbVirtualBus::UsbHciGetRequestSize() { return Request::RequestSize(sizeof(usb_request_t)); }

void UsbVirtualEp::RequestComplete(zx_status_t status, size_t actual, RequestVariant request) {
  if (std::holds_alternative<usb::BorrowedRequest<void>>(request)) {
    std::get<usb::BorrowedRequest<void>>(request).Complete(status, actual);
    return;
  }

  if (bus_->device_dispatcher() != fdf::Dispatcher::GetCurrent()->async_dispatcher()) {
    libsync::Completion wait;
    async::PostTask(bus_->device_dispatcher(), [this, status, actual, &request, &wait]() {
      bus_->host_->ep(index_)->RequestComplete(status, actual,
                                               std::move(std::get<usb::FidlRequest>(request)));
      wait.Signal();
    });
    wait.Wait();
  } else {
    bus_->host_->ep(index_)->RequestComplete(status, actual,
                                             std::move(std::get<usb::FidlRequest>(request)));
  }
}

void UsbVirtualBus::Enable(EnableCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);

  zx_status_t status = ZX_OK;

  if (host_ == nullptr) {
    status = CreateHost();
  }
  if (status == ZX_OK && device_ == nullptr) {
    status = CreateDevice();
  }

  completer.Reply(status);
}

void UsbVirtualBus::Disable(DisableCompleter::Sync& completer) {
  if (disable_completer_) {
    completer.Reply(ZX_ERR_BAD_STATE);
    return;
  }

  disable_completer_ = completer.ToAsync();

  int rc = thrd_create_with_name(
      &disable_thread_,
      [](void* arg) -> int {
        auto virtual_bus = reinterpret_cast<UsbVirtualBus*>(arg);
        virtual_bus->SetConnected(false);
        UsbVirtualHost* host;
        UsbVirtualDevice* device;
        {
          fbl::AutoLock lock(&virtual_bus->lock_);
          // Use release() here to avoid double free of these objects.
          // devmgr will handle freeing them.
          host = virtual_bus->host_.release();
          device = virtual_bus->device_.release();
        }
        if (host) {
          host->DdkAsyncRemove();
        }
        if (device) {
          device->DdkAsyncRemove();
        }
        virtual_bus->disable_completer_->Reply(ZX_OK);
        virtual_bus->disable_completer_.reset();
        return 0;
      },
      reinterpret_cast<void*>(this), "usb-virtual-bus-disable-thread");

  if (rc != thrd_success) {
    disable_completer_->Reply(ZX_ERR_INTERNAL);
    return;
  }
}

void UsbVirtualBus::Connect(ConnectCompleter::Sync& completer) {
  if (host_ == nullptr || device_ == nullptr) {
    completer.Reply(ZX_ERR_BAD_STATE);
    return;
  }

  SetConnected(true);
  completer.Reply(ZX_OK);
}

void UsbVirtualBus::Disconnect(DisconnectCompleter::Sync& completer) {
  if (host_ == nullptr || device_ == nullptr || disconnect_completer_) {
    completer.Reply(ZX_ERR_BAD_STATE);
    return;
  }

  disconnect_completer_ = completer.ToAsync();

  int rc = thrd_create_with_name(
      &disconnect_thread_,
      [](void* arg) -> int {
        auto virtual_bus = reinterpret_cast<UsbVirtualBus*>(arg);
        virtual_bus->SetConnected(false);
        virtual_bus->disconnect_completer_->Reply(ZX_OK);
        virtual_bus->disconnect_completer_.reset();
        return 0;
      },
      reinterpret_cast<void*>(this), "usb-virtual-bus-disconnect-thread");

  if (rc != thrd_success) {
    disconnect_completer_->Reply(ZX_ERR_INTERNAL);
    return;
  }
}

static zx_status_t usb_virtual_bus_bind(void* ctx, zx_device_t* parent) {
  return usb_virtual_bus::UsbVirtualBus::Create(parent);
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = usb_virtual_bus_bind;
  return ops;
}();

}  // namespace usb_virtual_bus

ZIRCON_DRIVER(usb_virtual_bus, usb_virtual_bus::driver_ops, "zircon", "0.1");
