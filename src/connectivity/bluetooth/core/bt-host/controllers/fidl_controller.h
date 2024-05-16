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

class HciEventHandler : public fidl::AsyncEventHandler<fuchsia_hardware_bluetooth::Hci> {
 public:
  HciEventHandler(std::function<void(zx_status_t)> unbind_callback);
  void on_fidl_error(fidl::UnbindInfo error) override;
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_hardware_bluetooth::Hci> metadata) override;

 private:
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
  // When both |get_features_callback_| and |vendor_features_| have values, call
  // |get_features_callback_| to report the stored features.
  void ReportVendorFeaturesIfAvailable();

  // Cleanup and call |error_cb_| with |status|
  void OnError(zx_status_t status);

  void CleanUp();

  void InitializeWait(async::WaitBase& wait, zx::channel& channel);

  void OnChannelSignal(const char* chan_name, async::WaitBase* wait, pw::span<std::byte> buffer,
                       zx::channel& channel, DataFunction& data_cb);

  void OnAclSignal(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                   const zx_packet_signal_t* signal);

  void OnCommandSignal(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                       const zx_packet_signal_t* signal);

  void OnScoSignal(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                   const zx_packet_signal_t* signal);

  void OnIsoSignal(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                   const zx_packet_signal_t* signal);

  // Initializes HCI layer by binding |hci_handle| to |hci_| and opening two-way command channel and
  // ACL data channel
  void InitializeHci(fidl::ClientEnd<fuchsia_hardware_bluetooth::Hci> hci_client_end);

  // |vendor_handle_| holds the Vendor channel until Initialize() is called, at which point
  // |vendor_| is bound to the channel. This prevents errors from being lost before initialization.
  fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor> vendor_client_end_;
  fidl::Client<fuchsia_hardware_bluetooth::Vendor> vendor_;

  fidl::Client<fuchsia_hardware_bluetooth::Hci> hci_;

  VendorEventHandler vendor_event_handler_;
  HciEventHandler hci_event_handler_;

  // |get_features_callback_| stores the callback that current object receives from GetFeatures
  // call. |vendor_features_| stores the features that are reported from the vendor driver through
  // fuchsia_hardware_bluetooth::Vendor::OnVendorConnected event.
  std::optional<pw::Callback<void(FidlController::FeaturesBits)>> get_features_callback_;
  std::optional<fuchsia_hardware_bluetooth::VendorFeatures> vendor_features_;

  async_dispatcher_t* dispatcher_;

  zx::channel acl_channel_;
  zx::channel command_channel_;
  zx::channel sco_channel_;
  zx::channel iso_channel_;

  DataFunction event_cb_;
  DataFunction acl_cb_;
  DataFunction sco_cb_;
  DataFunction iso_cb_;
  PwStatusCallback initialize_complete_cb_;
  PwStatusCallback error_cb_;

  async::WaitMethod<FidlController, &FidlController::OnAclSignal> acl_wait_{this};
  async::WaitMethod<FidlController, &FidlController::OnCommandSignal> command_wait_{this};
  async::WaitMethod<FidlController, &FidlController::OnScoSignal> sco_wait_{this};
  async::WaitMethod<FidlController, &FidlController::OnIsoSignal> iso_wait_{this};

  bool shutting_down_ = false;
};

}  // namespace bt::controllers

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_CONTROLLERS_FIDL_CONTROLLER_H_
