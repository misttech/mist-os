// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fidl_controller.h"

#include "fuchsia/hardware/bluetooth/cpp/fidl.h"
#include "helpers.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/assert.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/log.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/trace.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/slab_allocators.h"
#include "zircon/status.h"

namespace bt::controllers {

namespace fhbt = fuchsia::hardware::bluetooth;

namespace {
pw::bluetooth::Controller::FeaturesBits VendorFeaturesToFeaturesBits(
    fhbt::BtVendorFeatures features) {
  pw::bluetooth::Controller::FeaturesBits out{0};
  if (features & fhbt::BtVendorFeatures::SET_ACL_PRIORITY_COMMAND) {
    out |= pw::bluetooth::Controller::FeaturesBits::kSetAclPriorityCommand;
  }
  if (features & fhbt::BtVendorFeatures::ANDROID_VENDOR_EXTENSIONS) {
    out |= pw::bluetooth::Controller::FeaturesBits::kAndroidVendorExtensions;
  }
  return out;
}

fhbt::BtVendorAclPriority AclPriorityToFidl(pw::bluetooth::AclPriority priority) {
  switch (priority) {
    case pw::bluetooth::AclPriority::kNormal:
      return fhbt::BtVendorAclPriority::NORMAL;
    case pw::bluetooth::AclPriority::kSource:
    case pw::bluetooth::AclPriority::kSink:
      return fhbt::BtVendorAclPriority::HIGH;
  }
}

fhbt::BtVendorAclDirection AclPriorityToFidlAclDirection(pw::bluetooth::AclPriority priority) {
  switch (priority) {
    // The direction for kNormal is arbitrary.
    case pw::bluetooth::AclPriority::kNormal:
    case pw::bluetooth::AclPriority::kSource:
      return fhbt::BtVendorAclDirection::SOURCE;
    case pw::bluetooth::AclPriority::kSink:
      return fhbt::BtVendorAclDirection::SINK;
  }
}
}  // namespace

FidlController::FidlController(fhbt::VendorHandle vendor, async_dispatcher_t* dispatcher)
    : vendor_handle_(std::move(vendor)), dispatcher_(dispatcher) {
  BT_ASSERT(vendor_handle_.is_valid());
}

FidlController::~FidlController() { CleanUp(); }

void FidlController::Initialize(PwStatusCallback complete_callback,
                                PwStatusCallback error_callback) {
  initialize_complete_cb_ = std::move(complete_callback);
  error_cb_ = std::move(error_callback);

  vendor_ = vendor_handle_.Bind();
  vendor_.set_error_handler([this](zx_status_t status) {
    bt_log(ERROR, "controllers", "BtVendor protocol closed: %s", zx_status_get_string(status));
    OnError(status);
  });

  // Connect to Hci protocol
  vendor_->OpenHci([this](fuchsia::hardware::bluetooth::Vendor_OpenHci_Result result) {
    if (result.is_err()) {
      bt_log(ERROR, "bt-host", "Failed to open Hci: %s", zx_status_get_string(result.err()));
      OnError(result.err());
      return;
    }
    fuchsia::hardware::bluetooth::HciHandle hci_handle =
        fuchsia::hardware::bluetooth::HciHandle(std::move(result.response().channel));

    InitializeHci(std::move(hci_handle));
  });
}

void FidlController::InitializeHci(fuchsia::hardware::bluetooth::HciHandle hci_handle) {
  // We wait to bind hci_ until initialization because otherwise errors are dropped if the async
  // loop runs between Bind() and set_error_handle(). set_error_handler() will not be called
  // synchronously, so there is no risk that OnError() is called immediately.
  hci_ = hci_handle.Bind();
  hci_.set_error_handler([this](zx_status_t status) {
    bt_log(ERROR, "controllers", "BtHci protocol closed: %s", zx_status_get_string(status));
    OnError(status);
  });

  zx::channel their_command_chan;
  zx_status_t status = zx::channel::create(0, &command_channel_, &their_command_chan);
  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "Failed to create command channel");
    OnError(status);
    return;
  }

  hci_->OpenCommandChannel(std::move(their_command_chan),
                           [](fhbt::Hci_OpenCommandChannel_Result result) {
                             if (result.is_err()) {
                               bt_log(ERROR, "controllers", "Failed to open command channel: %s",
                                      zx_status_get_string(result.err()));
                             }
                           });
  InitializeWait(command_wait_, command_channel_);

  zx::channel their_acl_chan;
  status = zx::channel::create(0, &acl_channel_, &their_acl_chan);
  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "Failed to create ACL channel");
    OnError(status);
    return;
  }

  hci_->OpenAclDataChannel(std::move(their_acl_chan),
                           [](fhbt::Hci_OpenAclDataChannel_Result result) {
                             if (result.is_err()) {
                               bt_log(ERROR, "controllers", "Failed to open ACL data channel: %s",
                                      zx_status_get_string(result.err()));
                             }
                           });
  InitializeWait(acl_wait_, acl_channel_);

  initialize_complete_cb_(PW_STATUS_OK);
}

void FidlController::Close(PwStatusCallback callback) {
  CleanUp();
  callback(PW_STATUS_OK);
}

void FidlController::SendCommand(pw::span<const std::byte> command) {
  zx_status_t status =
      command_channel_.write(/*flags=*/0, command.data(), static_cast<uint32_t>(command.size()),
                             /*handles=*/nullptr, /*num_handles=*/0);
  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "failed to write command channel: %s",
           zx_status_get_string(status));
    OnError(status);
  }
}

void FidlController::SendAclData(pw::span<const std::byte> data) {
  zx_status_t status =
      acl_channel_.write(/*flags=*/0, data.data(), static_cast<uint32_t>(data.size()),
                         /*handles=*/nullptr, /*num_handles=*/0);
  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "failed to write ACL channel: %s", zx_status_get_string(status));
    OnError(status);
  }
}

void FidlController::GetFeatures(pw::Callback<void(FidlController::FeaturesBits)> callback) {
  if (!vendor_) {
    callback(pw::bluetooth::Controller::FeaturesBits{0});
    return;
  }

  vendor_->GetFeatures([cb = std::move(callback)](fhbt::Vendor_GetFeatures_Result result) mutable {
    cb(VendorFeaturesToFeaturesBits(result.response().features));
  });
}

void FidlController::EncodeVendorCommand(
    pw::bluetooth::VendorCommandParameters parameters,
    pw::Callback<void(pw::Result<pw::span<const std::byte>>)> callback) {
  BT_ASSERT(vendor_);

  if (!std::holds_alternative<pw::bluetooth::SetAclPriorityCommandParameters>(parameters)) {
    callback(pw::Status::Unimplemented());
    return;
  }

  pw::bluetooth::SetAclPriorityCommandParameters params =
      std::get<pw::bluetooth::SetAclPriorityCommandParameters>(parameters);

  fhbt::BtVendorSetAclPriorityParams priority_params;
  priority_params.connection_handle = params.connection_handle;
  priority_params.priority = AclPriorityToFidl(params.priority);
  priority_params.direction = AclPriorityToFidlAclDirection(params.priority);

  fhbt::BtVendorCommand command;
  command.set_set_acl_priority(priority_params);

  vendor_->EncodeCommand(std::move(command), [cb = std::move(callback)](
                                                 fhbt::Vendor_EncodeCommand_Result result) mutable {
    if (result.is_err()) {
      bt_log(ERROR, "controllers", "Failed to encode vendor command: %s",
             zx_status_get_string(result.err()));
      cb(ZxStatusToPwStatus(result.err()));
      return;
    }
    auto span = pw::as_bytes(pw::span(result.response().encoded));
    cb(span);
  });
}

void FidlController::OnError(zx_status_t status) {
  CleanUp();

  hci_.Unbind();
  vendor_.Unbind();

  // If |initialize_complete_cb_| has already been called, then initialization is complete and we
  // use |error_cb_|
  if (initialize_complete_cb_) {
    initialize_complete_cb_(ZxStatusToPwStatus(status));
  } else if (error_cb_) {
    error_cb_(ZxStatusToPwStatus(status));
  }
}

void FidlController::CleanUp() {
  // Waits need to be canceled before the underlying channels are destroyed.
  acl_wait_.Cancel();
  command_wait_.Cancel();

  acl_channel_.reset();
  command_channel_.reset();
}

void FidlController::InitializeWait(async::WaitBase& wait, zx::channel& channel) {
  BT_ASSERT(channel.is_valid());
  wait.Cancel();
  wait.set_object(channel.get());
  wait.set_trigger(ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED);
  BT_ASSERT(wait.Begin(dispatcher_) == ZX_OK);
}

void FidlController::OnChannelSignal(const char* chan_name, zx_status_t status,
                                     async::WaitBase* wait, const zx_packet_signal_t* signal,
                                     pw::span<std::byte> buffer, zx::channel& channel,
                                     DataFunction& data_cb) {
  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "%s channel error: %s", chan_name, zx_status_get_string(status));
    OnError(status);
    return;
  }

  if (signal->observed & ZX_CHANNEL_PEER_CLOSED) {
    bt_log(ERROR, "controllers", "%s channel closed", chan_name);
    OnError(ZX_ERR_PEER_CLOSED);
    return;
  }
  BT_ASSERT(signal->observed & ZX_CHANNEL_READABLE);

  uint32_t read_size;
  zx_status_t read_status = channel.read(0u, buffer.data(), /*handles=*/nullptr,
                                         static_cast<uint32_t>(buffer.size()), 0, &read_size,
                                         /*actual_handles=*/nullptr);
  if (read_status != ZX_OK) {
    bt_log(ERROR, "controllers", "%s channel: failed to read RX bytes: %s", chan_name,
           zx_status_get_string(read_status));
    OnError(read_status);
    return;
  }
  if (data_cb) {
    data_cb(buffer.subspan(0, read_size));
  } else {
    bt_log(WARN, "controllers", "Dropping packet received on %s channel (no rx callback set)",
           chan_name);
  }

  // The wait needs to be restarted after every signal.
  status = wait->Begin(dispatcher_);
  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "%s wait error: %s", chan_name, zx_status_get_string(status));
    OnError(status);
  }
}

void FidlController::OnAclSignal(async_dispatcher_t* /*dispatcher*/, async::WaitBase* wait,
                                 zx_status_t status, const zx_packet_signal_t* signal) {
  TRACE_DURATION("bluetooth", "FidlController::OnAclSignal");

  // Allocate a buffer for the packet. Since we don't know the size beforehand we allocate the
  // largest possible buffer.
  std::byte packet[hci::allocators::kLargeACLDataPacketSize];
  OnChannelSignal("ACL", status, wait, signal, packet, acl_channel_, acl_cb_);
}

void FidlController::OnCommandSignal(async_dispatcher_t* /*dispatcher*/, async::WaitBase* wait,
                                     zx_status_t status, const zx_packet_signal_t* signal) {
  TRACE_DURATION("bluetooth", "FidlController::OnCommandSignal");

  // Allocate a buffer for the packet. Since we don't know the size beforehand we allocate the
  // largest possible buffer.
  std::byte packet[hci_spec::kMaxEventPacketPayloadSize + sizeof(hci_spec::EventHeader)];
  OnChannelSignal("command", status, wait, signal, packet, command_channel_, event_cb_);
}

}  // namespace bt::controllers
