// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_CONTROLLERS_FIDL_CONTROLLER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_CONTROLLERS_FIDL_CONTROLLER_H_

#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/dispatcher.h>
#include <lib/zx/channel.h>

#include "pw_bluetooth/controller.h"

namespace bt::controllers {

class VendorEventHandler : public fidl::AsyncEventHandler<fuchsia_hardware_bluetooth::Vendor> {
 public:
  VendorEventHandler(
      std::function<void(zx_status_t)> unbind_callback,
      std::function<void(fuchsia_hardware_bluetooth::VendorFeatures)> on_vendor_connected_callback);
  void on_fidl_error(fidl::UnbindInfo error) override;
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_hardware_bluetooth::Vendor> metadata) override;

  void OnFeatures(fidl::Event<fuchsia_hardware_bluetooth::Vendor::OnFeatures>& event) override;

 private:
  std::function<void(zx_status_t)> unbind_callback_;
  std::function<void(fuchsia_hardware_bluetooth::VendorFeatures)> on_vendor_connected_callback_;
};

class HciEventHandler : public fidl::AsyncEventHandler<fuchsia_hardware_bluetooth::HciTransport> {
 public:
  HciEventHandler(
      std::function<void(zx_status_t)> unbind_callback,
      std::function<void(fuchsia_hardware_bluetooth::ReceivedPacket)> on_receive_callback);
  void OnReceive(fuchsia_hardware_bluetooth::ReceivedPacket&) override;
  void on_fidl_error(fidl::UnbindInfo error) override;
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_hardware_bluetooth::HciTransport> metadata) override;

 private:
  std::function<void(fuchsia_hardware_bluetooth::ReceivedPacket)> on_receive_callback_;
  std::function<void(zx_status_t)> unbind_callback_;
};

class FidlController final : public pw::bluetooth::Controller {
 public:
  using PwStatusCallback = pw::Callback<void(pw::Status)>;

  // |dispatcher| must outlive this object.
  FidlController(fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor> vendor_client_end,
                 async_dispatcher_t* dispatcher);

  ~FidlController() override;

  // Controller overrides:
  void SetEventFunction(DataFunction func) override { event_cb_ = std::move(func); }

  void SetReceiveAclFunction(DataFunction func) override { acl_cb_ = std::move(func); }

  void SetReceiveScoFunction(DataFunction func) override { sco_cb_ = std::move(func); }

  void SetReceiveIsoFunction(DataFunction func) override { iso_cb_ = std::move(func); }

  void Initialize(PwStatusCallback complete_callback, PwStatusCallback error_callback) override;

  void Close(PwStatusCallback callback) override;

  void SendCommand(pw::span<const std::byte> command) override;

  void SendAclData(pw::span<const std::byte> data) override;

  void SendScoData(pw::span<const std::byte> data) override;

  void SendIsoData(pw::span<const std::byte> data) override;

  void ConfigureSco(ScoCodingFormat coding_format, ScoEncoding encoding, ScoSampleRate sample_rate,
                    pw::Callback<void(pw::Status)> callback) override;

  void ResetSco(pw::Callback<void(pw::Status)> callback) override;

  void GetFeatures(pw::Callback<void(FeaturesBits)> callback) override;
  void EncodeVendorCommand(
      pw::bluetooth::VendorCommandParameters parameters,
      pw::Callback<void(pw::Result<pw::span<const std::byte>>)> callback) override;

 private:
  class ScoEventHandler
      : public fidl::AsyncEventHandler<fuchsia_hardware_bluetooth::ScoConnection> {
   public:
    ScoEventHandler(pw::Function<void(zx_status_t)> unbind_callback,
                    pw::Function<void(fuchsia_hardware_bluetooth::ScoPacket)> on_receive_callback);

   private:
    // AsyncEventHandler<ScoConnection> overrides:
    void OnReceive(fuchsia_hardware_bluetooth::ScoPacket& packet) override;
    void on_fidl_error(fidl::UnbindInfo error) override;
    void handle_unknown_event(
        fidl::UnknownEventMetadata<fuchsia_hardware_bluetooth::ScoConnection> metadata) override;

    pw::Function<void(fuchsia_hardware_bluetooth::ScoPacket)> on_receive_callback_;
    pw::Function<void(zx_status_t)> unbind_callback_;
  };

  void OnReceive(fuchsia_hardware_bluetooth::ReceivedPacket packet);
  void OnReceiveSco(fuchsia_hardware_bluetooth::ScoPacket packet);

  void OnScoUnbind(zx_status_t status);

  // When both |get_features_callback_| and |vendor_features_| have values, call
  // |get_features_callback_| to report the stored features.
  void ReportVendorFeaturesIfAvailable();

  // Cleanup and call |error_cb_| with |status|
  void OnError(zx_status_t status);

  void CleanUp();

  // Initializes HCI layer by binding |hci_handle| to |hci_| and opening two-way command channel and
  // ACL data channel
  void InitializeHci(fidl::ClientEnd<fuchsia_hardware_bluetooth::HciTransport> hci_client_end);

  // |vendor_handle_| holds the Vendor channel until Initialize() is called, at which point
  // |vendor_| is bound to the channel. This prevents errors from being lost before initialization.
  fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor> vendor_client_end_;
  fidl::Client<fuchsia_hardware_bluetooth::Vendor> vendor_;

  fidl::Client<fuchsia_hardware_bluetooth::HciTransport> hci_;

  VendorEventHandler vendor_event_handler_;
  HciEventHandler hci_event_handler_;

  // Only set after ConfigureSco() is called. Unbound on ResetSco().
  std::optional<fidl::Client<fuchsia_hardware_bluetooth::ScoConnection>> sco_connection_;
  // Shared across all ScoConnections.
  ScoEventHandler sco_event_handler_;
  // Valid only when a ResetSco() call is pending.
  PwStatusCallback reset_sco_cb_;

  // |get_features_callback_| stores the callback that current object receives from GetFeatures
  // call. |vendor_features_| stores the features that are reported from the vendor driver through
  // fuchsia_hardware_bluetooth::Vendor::OnVendorConnected event.
  std::optional<pw::Callback<void(FidlController::FeaturesBits)>> get_features_callback_;
  std::optional<fuchsia_hardware_bluetooth::VendorFeatures> vendor_features_;

  async_dispatcher_t* dispatcher_;

  DataFunction event_cb_;
  DataFunction acl_cb_;
  DataFunction sco_cb_;
  DataFunction iso_cb_;
  PwStatusCallback initialize_complete_cb_;
  PwStatusCallback error_cb_;

  bool shutting_down_ = false;
};

}  // namespace bt::controllers

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_CONTROLLERS_FIDL_CONTROLLER_H_
