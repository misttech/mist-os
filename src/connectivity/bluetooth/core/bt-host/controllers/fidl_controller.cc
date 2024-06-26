// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fidl_controller.h"

#include "helpers.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/assert.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/log.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/trace.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/iso/iso_common.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/slab_allocators.h"
#include "zircon/status.h"

namespace bt::controllers {

namespace fhbt = fuchsia_hardware_bluetooth;

namespace {
pw::bluetooth::Controller::FeaturesBits VendorFeaturesToFeaturesBits(
    fhbt::VendorFeatures features) {
  pw::bluetooth::Controller::FeaturesBits out{0};
  if (features.acl_priority_command().has_value() && features.acl_priority_command()) {
    out |= pw::bluetooth::Controller::FeaturesBits::kSetAclPriorityCommand;
  }
  if (features.android_vendor_extensions().has_value()) {
    // Ignore the content of android_vendor_extension field now.
    out |= pw::bluetooth::Controller::FeaturesBits::kAndroidVendorExtensions;
  }
  return out;
}

fhbt::VendorAclPriority AclPriorityToFidl(pw::bluetooth::AclPriority priority) {
  switch (priority) {
    case pw::bluetooth::AclPriority::kNormal:
      return fhbt::VendorAclPriority::kNormal;
    case pw::bluetooth::AclPriority::kSource:
    case pw::bluetooth::AclPriority::kSink:
      return fhbt::VendorAclPriority::kHigh;
  }
}

fhbt::VendorAclDirection AclPriorityToFidlAclDirection(pw::bluetooth::AclPriority priority) {
  switch (priority) {
    // The direction for kNormal is arbitrary.
    case pw::bluetooth::AclPriority::kNormal:
    case pw::bluetooth::AclPriority::kSource:
      return fhbt::VendorAclDirection::kSource;
    case pw::bluetooth::AclPriority::kSink:
      return fhbt::VendorAclDirection::kSink;
  }
}

fhbt::ScoCodingFormat ScoCodingFormatToFidl(
    pw::bluetooth::Controller::ScoCodingFormat coding_format) {
  switch (coding_format) {
    case pw::bluetooth::Controller::ScoCodingFormat::kCvsd:
      return fhbt::ScoCodingFormat::kCvsd;
    case pw::bluetooth::Controller::ScoCodingFormat::kMsbc:
      return fhbt::ScoCodingFormat::kMsbc;
    default:
      BT_PANIC("invalid SCO coding format");
  }
}

fhbt::ScoEncoding ScoEncodingToFidl(pw::bluetooth::Controller::ScoEncoding encoding) {
  switch (encoding) {
    case pw::bluetooth::Controller::ScoEncoding::k8Bits:
      return fhbt::ScoEncoding::kBits8;
    case pw::bluetooth::Controller::ScoEncoding::k16Bits:
      return fhbt::ScoEncoding::kBits16;
    default:
      BT_PANIC("invalid SCO encoding");
  }
}

fhbt::ScoSampleRate ScoSampleRateToFidl(pw::bluetooth::Controller::ScoSampleRate sample_rate) {
  switch (sample_rate) {
    case pw::bluetooth::Controller::ScoSampleRate::k8Khz:
      return fhbt::ScoSampleRate::kKhz8;
    case pw::bluetooth::Controller::ScoSampleRate::k16Khz:
      return fhbt::ScoSampleRate::kKhz16;
    default:
      BT_PANIC("invalid SCO sample rate");
  }
}

}  // namespace

VendorEventHandler::VendorEventHandler(
    std::function<void(zx_status_t)> unbind_callback,
    std::function<void(fhbt::VendorFeatures)> on_vendor_connected_callback)
    : unbind_callback_(unbind_callback),
      on_vendor_connected_callback_(on_vendor_connected_callback) {}

void VendorEventHandler::handle_unknown_event(fidl::UnknownEventMetadata<fhbt::Vendor> metadata) {
  bt_log(WARN, "controllers", "Unknown event from Vendor server: %lu", metadata.event_ordinal);
}

void VendorEventHandler::on_fidl_error(fidl::UnbindInfo error) {
  bt_log(ERROR, "controllers", "Vendor protocol closed: %s", error.FormatDescription().c_str());
  unbind_callback_(ZX_ERR_PEER_CLOSED);
}

void VendorEventHandler::OnFeatures(fidl::Event<fhbt::Vendor::OnFeatures>& event) {
  on_vendor_connected_callback_(event);
}

HciEventHandler::HciEventHandler(std::function<void(zx_status_t)> unbind_callback)
    : unbind_callback_(unbind_callback) {}

void HciEventHandler::handle_unknown_event(fidl::UnknownEventMetadata<fhbt::Hci> metadata) {
  bt_log(WARN, "controllers", "Unknown event from Hci server: %lu", metadata.event_ordinal);
}

void HciEventHandler::on_fidl_error(fidl::UnbindInfo error) {
  bt_log(ERROR, "controllers", "Hci protocol closed: %s", error.FormatDescription().c_str());
  unbind_callback_(ZX_ERR_PEER_CLOSED);
}

FidlController::FidlController(fidl::ClientEnd<fhbt::Vendor> vendor_client_end,
                               async_dispatcher_t* dispatcher)
    : vendor_event_handler_([this](zx_status_t status) { OnError(status); },
                            [this](fhbt::VendorFeatures features) {
                              vendor_features_ = features;
                              ReportVendorFeaturesIfAvailable();
                            }),
      hci_event_handler_([this](zx_status_t status) { OnError(status); }),
      dispatcher_(dispatcher) {
  BT_ASSERT(vendor_client_end.is_valid());
  vendor_client_end_ = std::move(vendor_client_end);
}

FidlController::~FidlController() { CleanUp(); }

void FidlController::Initialize(PwStatusCallback complete_callback,
                                PwStatusCallback error_callback) {
  initialize_complete_cb_ = std::move(complete_callback);
  error_cb_ = std::move(error_callback);

  vendor_ = fidl::Client<fhbt::Vendor>(std::move(vendor_client_end_), dispatcher_,
                                       &vendor_event_handler_);

  // Connect to Hci protocol
  vendor_->OpenHci().ThenExactlyOnce([this](fidl::Result<fhbt::Vendor::OpenHci>& result) {
    if (!result.is_ok()) {
      bt_log(ERROR, "bt-host", "Failed to open Hci: %s",
             result.error_value().FormatDescription().c_str());
      if (result.error_value().is_framework_error()) {
        OnError(result.error_value().framework_error().status());
      } else {
        OnError(result.error_value().domain_error());
      }
      return;
    }

    InitializeHci(std::move(result->channel()));
  });
}

void FidlController::InitializeHci(fidl::ClientEnd<fhbt::Hci> hci_client_end) {
  hci_ = fidl::Client<fhbt::Hci>(std::move(hci_client_end), dispatcher_, &hci_event_handler_);

  zx::channel their_command_chan;
  zx_status_t status = zx::channel::create(0, &command_channel_, &their_command_chan);
  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "Failed to create command channel");
    OnError(status);
    return;
  }
  hci_->OpenCommandChannel(std::move(their_command_chan))
      .ThenExactlyOnce([](fidl::Result<fhbt::Hci::OpenCommandChannel>& result) {
        if (!result.is_ok()) {
          bt_log(ERROR, "controllers", "Failed to open command channel: %s",
                 result.error_value().FormatDescription().c_str());
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
  hci_->OpenAclDataChannel(std::move(their_acl_chan))
      .ThenExactlyOnce([](fidl::Result<fhbt::Hci::OpenAclDataChannel>& result) {
        if (!result.is_ok()) {
          bt_log(ERROR, "controllers", "Failed to open ACL data channel: %s",
                 result.error_value().FormatDescription().c_str());
        }
      });
  InitializeWait(acl_wait_, acl_channel_);

  zx::channel their_sco_chan;
  status = zx::channel::create(0, &sco_channel_, &their_sco_chan);
  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "Failed to create SCO channel");
    OnError(status);
    return;
  }
  hci_->OpenScoDataChannel(std::move(their_sco_chan))
      .ThenExactlyOnce([this](fidl::Result<fhbt::Hci::OpenScoDataChannel>& result) {
        if (!result.is_ok()) {
          // Non-fatal - may simply indicate a lack of SCO data support
          // in the driver.
          bt_log(INFO, "controllers", "Failed to open SCO data channel: %s",
                 result.error_value().FormatDescription().c_str());
          sco_channel_.reset();
        } else {
          InitializeWait(sco_wait_, sco_channel_);
        }
      });

  zx::channel their_iso_chan;
  status = zx::channel::create(0, &iso_channel_, &their_iso_chan);
  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "Failed to create ISO channel");
    OnError(status);
    return;
  }
  hci_->OpenIsoDataChannel(std::move(their_iso_chan))
      .ThenExactlyOnce([this](fidl::Result<fhbt::Hci::OpenIsoDataChannel>& result) {
        if (!result.is_ok()) {
          // Non-fatal - may simply indicate a lack of ISO data support
          // in the driver.
          bt_log(INFO, "controllers", "Failed to open ISO data channel: %s",
                 result.error_value().FormatDescription().c_str());
          iso_channel_.reset();
        } else {
          // Don't wait on channel signals until we have confirmation from
          // the driver that the channel has been established. b/328504823
          InitializeWait(iso_wait_, iso_channel_);
        }
      });

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
    return;
  }
}

void FidlController::SendAclData(pw::span<const std::byte> data) {
  zx_status_t status =
      acl_channel_.write(/*flags=*/0, data.data(), static_cast<uint32_t>(data.size()),
                         /*handles=*/nullptr, /*num_handles=*/0);
  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "failed to write ACL channel: %s", zx_status_get_string(status));
    OnError(status);
    return;
  }
}

void FidlController::SendScoData(pw::span<const std::byte> data) {
  zx_status_t status =
      sco_channel_.write(/*flags=*/0, data.data(), static_cast<uint32_t>(data.size()),
                         /*handles=*/nullptr, /*num_handles=*/0);

  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "failed to write SCO channel: %s", zx_status_get_string(status));
    OnError(status);
    return;
  }
}

void FidlController::SendIsoData(pw::span<const std::byte> data) {
  zx_status_t status =
      iso_channel_.write(/*flags=*/0, data.data(), static_cast<uint32_t>(data.size()),
                         /*handles=*/nullptr, /*num_handles=*/0);

  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "failed to write ISO channel: %s", zx_status_get_string(status));
    OnError(status);
    return;
  }
}

void FidlController::ConfigureSco(ScoCodingFormat coding_format, ScoEncoding encoding,
                                  ScoSampleRate sample_rate, PwStatusCallback callback) {
  hci_->ConfigureSco({ScoCodingFormatToFidl(coding_format), ScoEncodingToFidl(encoding),
                      ScoSampleRateToFidl(sample_rate)})
      .ThenExactlyOnce(
          [cb = std::move(callback)](fidl::Result<fhbt::Hci::ConfigureSco>& result) mutable {
            if (!result.is_ok()) {
              bt_log(ERROR, "controllers", "Failed to configure SCO: %s",
                     result.error_value().FormatDescription().c_str());
              if (result.error_value().is_framework_error()) {
                cb(ZxStatusToPwStatus(result.error_value().framework_error().status()));
              } else {
                cb(ZxStatusToPwStatus(result.error_value().domain_error()));
              }
              return;
            }
            cb(ZxStatusToPwStatus(ZX_OK));
          });
}

void FidlController::ResetSco(pw::Callback<void(pw::Status)> callback) {
  hci_->ResetSco().ThenExactlyOnce(
      [cb = std::move(callback)](fidl::Result<fhbt::Hci::ResetSco>& result) mutable {
        if (!result.is_ok()) {
          bt_log(ERROR, "controllers", "Failed to reset SCO: %s",
                 result.error_value().FormatDescription().c_str());
          if (result.error_value().is_framework_error()) {
            cb(ZxStatusToPwStatus(result.error_value().framework_error().status()));
          } else {
            cb(ZxStatusToPwStatus(result.error_value().domain_error()));
          }
          return;
        }
        cb(ZxStatusToPwStatus(ZX_OK));
      });
}

void FidlController::GetFeatures(pw::Callback<void(FidlController::FeaturesBits)> callback) {
  get_features_callback_ = std::move(callback);
  ReportVendorFeaturesIfAvailable();
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

  fhbt::VendorSetAclPriorityParams priority_params;
  priority_params.connection_handle(params.connection_handle);
  priority_params.priority(AclPriorityToFidl(params.priority));
  priority_params.direction(AclPriorityToFidlAclDirection(params.priority));

  auto command = fhbt::VendorCommand::WithSetAclPriority(priority_params);

  vendor_->EncodeCommand(std::move(command))
      .ThenExactlyOnce(
          [cb = std::move(callback)](fidl::Result<fhbt::Vendor::EncodeCommand>& result) mutable {
            if (!result.is_ok()) {
              bt_log(ERROR, "controllers", "Failed to encode vendor command: %s",
                     result.error_value().FormatDescription().c_str());
              if (result.error_value().is_framework_error()) {
                cb(ZxStatusToPwStatus(result.error_value().framework_error().status()));
              } else {
                cb(ZxStatusToPwStatus(result.error_value().domain_error()));
              }
              return;
            }
            auto span = pw::as_bytes(pw::span(result->encoded()));
            cb(span);
          });
}

void FidlController::ReportVendorFeaturesIfAvailable() {
  if (!get_features_callback_ || !vendor_features_) {
    return;
  }

  FidlController::FeaturesBits features_bits = VendorFeaturesToFeaturesBits(*vendor_features_);
  if (iso_channel_.is_valid()) {
    features_bits |= FeaturesBits::kHciIso;
  }

  (*get_features_callback_)(features_bits);
  get_features_callback_.reset();
  // Don't reset |vendor_features_| so that the controller reports the same features until it's
  // updated from the driver.
}

void FidlController::OnError(zx_status_t status) {
  CleanUp();
  if (hci_.is_valid())
    auto hci_endpoint = hci_.UnbindMaybeGetEndpoint();
  if (vendor_.is_valid())
    auto vendor_endpoint = vendor_.UnbindMaybeGetEndpoint();

  // If |initialize_complete_cb_| has already been called, then initialization is complete and we
  // use |error_cb_|
  if (initialize_complete_cb_) {
    initialize_complete_cb_(ZxStatusToPwStatus(status));
    // Clean up |error_cb_| since we only need one callback to be called during initialization.
    error_cb_ = nullptr;
  } else if (error_cb_) {
    error_cb_(ZxStatusToPwStatus(status));
  }
}

void FidlController::CleanUp() {
  if (shutting_down_) {
    return;
  }
  shutting_down_ = true;

  // Waits need to be canceled before the underlying channels are destroyed.
  acl_wait_.Cancel();
  sco_wait_.Cancel();
  iso_wait_.Cancel();
  command_wait_.Cancel();

  acl_channel_.reset();
  sco_channel_.reset();
  iso_channel_.reset();
  command_channel_.reset();
}

void FidlController::InitializeWait(async::WaitBase& wait, zx::channel& channel) {
  BT_ASSERT(channel.is_valid());
  wait.Cancel();
  wait.set_object(channel.get());
  wait.set_trigger(ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED);
  BT_ASSERT(wait.Begin(dispatcher_) == ZX_OK);
}

void FidlController::OnChannelSignal(const char* chan_name, async::WaitBase* wait,
                                     pw::span<std::byte> buffer, zx::channel& channel,
                                     DataFunction& data_cb) {
  uint32_t actual_size;
  zx_status_t read_status =
      channel.read(/*flags=*/0u, /*bytes=*/buffer.data(), /*handles=*/nullptr,
                   /*num_bytes=*/static_cast<uint32_t>(buffer.size()), /*num_handles=*/0,
                   /*actual_bytes=*/&actual_size,
                   /*actual_handles=*/nullptr);

  if (read_status != ZX_OK) {
    bt_log(ERROR, "controllers", "%s channel: failed to read RX bytes: %s", chan_name,
           zx_status_get_string(read_status));
    OnError(read_status);
    return;
  }

  if (data_cb) {
    data_cb(buffer.subspan(0, actual_size));
  } else {
    bt_log(WARN, "controllers", "Dropping packet received on %s channel (no rx callback set)",
           chan_name);
  }

  // The data callback above may trigger the controller to shut down (e.g., if it triggers a write
  // to a command channel as the response to a received event). Verify that the channel hasn't been
  // closed before we attempt to wait on it.
  if (shutting_down_) {
    return;
  }

  // The wait needs to be restarted after every signal.
  zx_status_t wait_status = wait->Begin(dispatcher_);
  if (wait_status != ZX_OK) {
    bt_log(ERROR, "controllers", "%s wait error: %s", chan_name, zx_status_get_string(wait_status));
    OnError(wait_status);
    return;
  }
}

void FidlController::OnAclSignal(async_dispatcher_t* /*dispatcher*/, async::WaitBase* wait,
                                 zx_status_t status, const zx_packet_signal_t* signal) {
  const char* kChannelName = "ACL";
  TRACE_DURATION("bluetooth", "FidlController::OnAclSignal");

  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "%s channel error: %s", kChannelName,
           zx_status_get_string(status));
    OnError(status);
    return;
  }
  if (signal->observed & ZX_CHANNEL_PEER_CLOSED) {
    bt_log(ERROR, "controllers", "%s channel closed", kChannelName);
    OnError(ZX_ERR_PEER_CLOSED);
    return;
  }
  BT_ASSERT(signal->observed & ZX_CHANNEL_READABLE);

  // Allocate a buffer for the packet. Since we don't know the size beforehand we allocate the
  // largest possible buffer.
  std::byte packet[hci::allocators::kLargeACLDataPacketSize];
  OnChannelSignal(kChannelName, wait, packet, acl_channel_, acl_cb_);
}

void FidlController::OnCommandSignal(async_dispatcher_t* /*dispatcher*/, async::WaitBase* wait,
                                     zx_status_t status, const zx_packet_signal_t* signal) {
  const char* kChannelName = "command";
  TRACE_DURATION("bluetooth", "FidlController::OnCommandSignal");

  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "%s channel error: %s", kChannelName,
           zx_status_get_string(status));
    OnError(status);
    return;
  }
  if (signal->observed & ZX_CHANNEL_PEER_CLOSED) {
    bt_log(ERROR, "controllers", "%s channel closed", kChannelName);
    OnError(ZX_ERR_PEER_CLOSED);
    return;
  }
  BT_ASSERT(signal->observed & ZX_CHANNEL_READABLE);

  // Allocate a buffer for the packet. Since we don't know the size beforehand we allocate the
  // largest possible buffer.
  constexpr uint32_t kMaxEventPacketSize =
      hci_spec::kMaxEventPacketPayloadSize + sizeof(hci_spec::EventHeader);
  std::byte packet[kMaxEventPacketSize];
  OnChannelSignal(kChannelName, wait, packet, command_channel_, event_cb_);
}

void FidlController::OnScoSignal(async_dispatcher_t* /*dispatcher*/, async::WaitBase* wait,
                                 zx_status_t status, const zx_packet_signal_t* signal) {
  const char* kChannelName = "SCO";
  TRACE_DURATION("bluetooth", "FidlController::OnScoSignal");

  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "%s channel error: %s", kChannelName,
           zx_status_get_string(status));
    OnError(status);
    return;
  }
  if (signal->observed & ZX_CHANNEL_PEER_CLOSED) {
    bt_log(ERROR, "controllers", "%s channel closed", kChannelName);
    OnError(ZX_ERR_PEER_CLOSED);
    return;
  }
  BT_ASSERT(signal->observed & ZX_CHANNEL_READABLE);

  // Allocate a buffer for the packet. Since we don't know the size beforehand we allocate the
  // largest possible buffer.
  constexpr uint32_t kMaxScoPacketSize =
      hci_spec::kMaxSynchronousDataPacketPayloadSize + sizeof(hci_spec::SynchronousDataHeader);
  std::byte packet[kMaxScoPacketSize];
  OnChannelSignal(kChannelName, wait, packet, sco_channel_, sco_cb_);
}

void FidlController::OnIsoSignal(async_dispatcher_t* /*dispatcher*/, async::WaitBase* wait,
                                 zx_status_t status, const zx_packet_signal_t* signal) {
  const char* kChannelName = "ISO";
  TRACE_DURATION("bluetooth", "FidlController::OnIsoSignal");

  if (status != ZX_OK) {
    bt_log(ERROR, "controllers", "%s channel error: %s", kChannelName,
           zx_status_get_string(status));
    OnError(status);
    return;
  }
  if (signal->observed & ZX_CHANNEL_PEER_CLOSED) {
    bt_log(ERROR, "controllers", "%s channel closed", kChannelName);
    OnError(ZX_ERR_PEER_CLOSED);
    return;
  }
  BT_ASSERT(signal->observed & ZX_CHANNEL_READABLE);

  // Isochronous data frames can be quite large (16KB), so dynamically allocate only what is needed.
  uint32_t read_size = 0;
  zx_status_t read_status =
      iso_channel_.read(/*flags=*/0u, /*bytes=*/nullptr, /*handles=*/nullptr, /*num_bytes=*/0u,
                        /*num_handles=*/0, &read_size, /*actual_handles=*/nullptr);
  if (read_status == ZX_OK) {
    bt_log(WARN, "controllers", "%s channel: read 0-length packet, ignoring", kChannelName);
    return;
  }
  if (read_status != ZX_ERR_BUFFER_TOO_SMALL) {
    bt_log(ERROR, "controllers", "%s channel: failed to read packet size: %s", kChannelName,
           zx_status_get_string(read_status));
    OnError(read_status);
    return;
  }
  if (read_size > iso::kMaxIsochronousDataPacketSize) {
    bt_log(ERROR, "controllers", "%s channel: packet size (%d) exceeds maximum (%zu)", kChannelName,
           read_size, iso::kMaxIsochronousDataPacketSize);
    OnError(read_status);
    return;
  }

  std::unique_ptr<std::byte[]> buffer_ptr = std::make_unique<std::byte[]>(read_size);
  pw::span<std::byte> packet{buffer_ptr.get(), read_size};
  OnChannelSignal(kChannelName, wait, packet, iso_channel_, iso_cb_);
}

}  // namespace bt::controllers
