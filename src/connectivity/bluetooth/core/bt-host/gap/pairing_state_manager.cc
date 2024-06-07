// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/pairing_state_manager.h"

#include <memory>
#include <utility>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/constants.h"

namespace bt::gap {

PairingStateManager::PairingStateManager(Peer::WeakPtr peer,
                                         WeakPtr<hci::BrEdrConnection> link,
                                         bool link_initiated,
                                         fit::closure auth_cb,
                                         StatusCallback status_cb)
    : peer_(std::move(peer)), link_(std::move(link)) {
  secure_simple_pairing_state_ = std::make_unique<SecureSimplePairingState>(
      peer_,
      link_,
      link_initiated,
      std::move(std::move(auth_cb)),
      std::move(std::move(status_cb)));
}

void PairingStateManager::InitiatePairing(
    BrEdrSecurityRequirements security_requirements, StatusCallback status_cb) {
  if (secure_simple_pairing_state_) {
    secure_simple_pairing_state_->InitiatePairing(security_requirements,
                                                  std::move(status_cb));
  }
}

std::optional<pw::bluetooth::emboss::IoCapability>
PairingStateManager::OnIoCapabilityRequest() {
  if (secure_simple_pairing_state_) {
    return secure_simple_pairing_state_->OnIoCapabilityRequest();
  }
  return std::nullopt;
}

void PairingStateManager::OnIoCapabilityResponse(
    pw::bluetooth::emboss::IoCapability peer_iocap) {
  if (secure_simple_pairing_state_) {
    secure_simple_pairing_state_->OnIoCapabilityResponse(peer_iocap);
  }
}

void PairingStateManager::OnUserConfirmationRequest(
    uint32_t numeric_value, UserConfirmationCallback cb) {
  if (secure_simple_pairing_state_) {
    secure_simple_pairing_state_->OnUserConfirmationRequest(numeric_value,
                                                            std::move(cb));
  }
}

void PairingStateManager::OnUserPasskeyRequest(UserPasskeyCallback cb) {
  if (secure_simple_pairing_state_) {
    secure_simple_pairing_state_->OnUserPasskeyRequest(std::move(cb));
  }
}

void PairingStateManager::OnUserPasskeyNotification(uint32_t numeric_value) {
  if (secure_simple_pairing_state_) {
    secure_simple_pairing_state_->OnUserPasskeyNotification(numeric_value);
  }
}

void PairingStateManager::OnSimplePairingComplete(
    pw::bluetooth::emboss::StatusCode status_code) {
  if (secure_simple_pairing_state_) {
    secure_simple_pairing_state_->OnSimplePairingComplete(status_code);
  }
}

std::optional<hci_spec::LinkKey> PairingStateManager::OnLinkKeyRequest() {
  if (secure_simple_pairing_state_) {
    return secure_simple_pairing_state_->OnLinkKeyRequest();
  }
  return std::nullopt;
}

void PairingStateManager::OnLinkKeyNotification(
    const UInt128& link_key,
    hci_spec::LinkKeyType key_type,
    bool local_secure_connections_supported) {
  if (secure_simple_pairing_state_) {
    secure_simple_pairing_state_->OnLinkKeyNotification(
        link_key, key_type, local_secure_connections_supported);
  }
}

void PairingStateManager::OnAuthenticationComplete(
    pw::bluetooth::emboss::StatusCode status_code) {
  if (secure_simple_pairing_state_) {
    secure_simple_pairing_state_->OnAuthenticationComplete(status_code);
  }
}

void PairingStateManager::OnEncryptionChange(hci::Result<bool> result) {
  if (secure_simple_pairing_state_) {
    secure_simple_pairing_state_->OnEncryptionChange(result);
  }
}

void PairingStateManager::AttachInspect(inspect::Node& parent,
                                        std::string name) {
  if (secure_simple_pairing_state_) {
    secure_simple_pairing_state_->AttachInspect(parent, std::move(name));
  }
}

}  // namespace bt::gap
