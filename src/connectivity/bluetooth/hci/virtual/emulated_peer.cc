// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "emulated_peer.h"

#include "src/connectivity/bluetooth/core/bt-host/fidl/helpers.h"

namespace fbt = fuchsia_bluetooth;
namespace ftest = fuchsia_bluetooth_test;

namespace bt_hci_virtual {
namespace {

bt::DeviceAddress::Type LeAddressTypeFromFidl(fbt::AddressType type) {
  return (type == fbt::AddressType::kRandom) ? bt::DeviceAddress::Type::kLERandom
                                             : bt::DeviceAddress::Type::kLEPublic;
}

bt::DeviceAddress LeAddressFromFidl(const fbt::Address& address) {
  return bt::DeviceAddress(LeAddressTypeFromFidl(address.type()), address.bytes());
}

pw::bluetooth::emboss::ConnectionRole ConnectionRoleFromFidl(fbt::ConnectionRole role) {
  switch (role) {
    case fbt::ConnectionRole::kLeader:
      return pw::bluetooth::emboss::ConnectionRole::CENTRAL;
    case fbt::ConnectionRole::kFollower:
      [[fallthrough]];
    default:
      break;
  }
  return pw::bluetooth::emboss::ConnectionRole::PERIPHERAL;
}

}  // namespace

// static
EmulatedPeer::Result EmulatedPeer::NewLowEnergy(ftest::LowEnergyPeerParameters parameters,
                                                fidl::ServerEnd<ftest::Peer> request,
                                                bt::testing::FakeController* fake_controller,
                                                async_dispatcher_t* dispatcher) {
  ZX_DEBUG_ASSERT(request);
  ZX_DEBUG_ASSERT(fake_controller);

  if (!parameters.address().has_value()) {
    bt_log(ERROR, "virtual", "A fake peer address is mandatory!\n");
    return fpromise::error(ftest::EmulatorPeerError::kParametersInvalid);
  }

  bt::BufferView adv, scan_response;
  if (parameters.advertisement().has_value()) {
    adv = bt::BufferView(parameters.advertisement()->data());
  }
  if (parameters.scan_response().has_value()) {
    scan_response = bt::BufferView(parameters.scan_response()->data());
  }

  auto address = LeAddressFromFidl(parameters.address().value());
  bool connectable = parameters.connectable().has_value() && parameters.connectable().value();
  bool scannable = scan_response.size() != 0u;

  // TODO(armansito): We should consider splitting bt::testing::FakePeer into separate types for
  // BR/EDR and LE transport emulation logic.
  auto peer = std::make_unique<bt::testing::FakePeer>(address, fake_controller->pw_dispatcher(),
                                                      connectable, scannable);
  peer->set_advertising_data(adv);
  if (scannable) {
    peer->set_scan_response(scan_response);
  }

  if (!fake_controller->AddPeer(std::move(peer))) {
    bt_log(ERROR, "virtual", "A fake LE peer with given address already exists: %s\n",
           address.ToString().c_str());
    return fpromise::error(ftest::EmulatorPeerError::kAddressRepeated);
  }

  return fpromise::ok(std::unique_ptr<EmulatedPeer>(
      new EmulatedPeer(address, std::move(request), fake_controller, dispatcher)));
}

// static
EmulatedPeer::Result EmulatedPeer::NewBredr(ftest::BredrPeerParameters parameters,
                                            fidl::ServerEnd<ftest::Peer> request,
                                            bt::testing::FakeController* fake_controller,
                                            async_dispatcher_t* dispatcher) {
  ZX_DEBUG_ASSERT(request);
  ZX_DEBUG_ASSERT(fake_controller);

  if (!parameters.address().has_value()) {
    bt_log(ERROR, "virtual", "A fake peer address is mandatory!\n");
    return fpromise::error(ftest::EmulatorPeerError::kParametersInvalid);
  }

  auto address = bt::DeviceAddress(bt::DeviceAddress::Type::kBREDR, parameters.address()->bytes());
  bool connectable = parameters.connectable().has_value() && parameters.connectable().value();

  // TODO(armansito): We should consider splitting bt::testing::FakePeer into separate types for
  // BR/EDR and LE transport emulation logic.
  auto peer = std::make_unique<bt::testing::FakePeer>(address, fake_controller->pw_dispatcher(),
                                                      connectable, false);
  if (parameters.device_class().has_value()) {
    peer->set_class_of_device(bt::DeviceClass(parameters.device_class()->value()));
  }
  if (parameters.service_definition().has_value()) {
    std::vector<bt::sdp::ServiceRecord> recs;
    for (const auto& defn : parameters.service_definition().value()) {
      auto rec = bthost::fidl_helpers::ServiceDefinitionToServiceRecord(defn);
      if (rec.is_ok()) {
        recs.emplace_back(std::move(rec.value()));
      }
    }
    bt::l2cap::ChannelParameters params;
    auto NopConnectCallback = [](auto /*channel*/, const bt::sdp::DataElement&) {};
    peer->sdp_server()->server()->RegisterService(std::move(recs), params, NopConnectCallback);
  }

  if (!fake_controller->AddPeer(std::move(peer))) {
    bt_log(ERROR, "virtual", "A fake BR/EDR peer with given address already exists: %s\n",
           address.ToString().c_str());
    return fpromise::error(ftest::EmulatorPeerError::kAddressRepeated);
  }

  return fpromise::ok(std::unique_ptr<EmulatedPeer>(
      new EmulatedPeer(address, std::move(request), fake_controller, dispatcher)));
}

EmulatedPeer::EmulatedPeer(bt::DeviceAddress address, fidl::ServerEnd<ftest::Peer> request,
                           bt::testing::FakeController* fake_controller,
                           async_dispatcher_t* dispatcher)
    : address_(address),
      fake_controller_(fake_controller),
      binding_(dispatcher, std::move(request), this, std::mem_fn(&EmulatedPeer::OnChannelClosed)) {
  ZX_DEBUG_ASSERT(fake_controller_);
}

EmulatedPeer::~EmulatedPeer() { CleanUp(); }

void EmulatedPeer::AssignConnectionStatus(AssignConnectionStatusRequest& request,
                                          AssignConnectionStatusCompleter::Sync& completer) {
  bt_log(TRACE, "virtual", "EmulatedPeer.AssignConnectionStatus\n");

  auto peer = fake_controller_->FindPeer(address_);
  if (peer) {
    peer->set_connect_response(static_cast<pw::bluetooth::emboss::StatusCode>(request.status()));
  }

  completer.Reply();
}

void EmulatedPeer::EmulateLeConnectionComplete(
    EmulateLeConnectionCompleteRequest& request,
    EmulateLeConnectionCompleteCompleter::Sync& completer) {
  bt_log(TRACE, "virtual", "EmulatedPeer.EmulateLeConnectionComplete\n");
  fake_controller_->ConnectLowEnergy(address_, ConnectionRoleFromFidl(request.role()));
}

void EmulatedPeer::EmulateDisconnectionComplete(
    EmulateDisconnectionCompleteCompleter::Sync& completer) {
  bt_log(TRACE, "virtual", "EmulatedPeer.EmulateDisconnectionComplete\n");
  fake_controller_->Disconnect(address_);
}

void EmulatedPeer::WatchConnectionStates(WatchConnectionStatesCompleter::Sync& completer) {
  bt_log(TRACE, "virtual", "EmulatedPeer.WatchConnectionState\n");

  {
    std::lock_guard<std::mutex> lock(connection_states_lock_);
    connection_states_completers_.emplace(completer.ToAsync());
  }
  MaybeUpdateConnectionStates();
}

void EmulatedPeer::UpdateConnectionState(bool connected) {
  ftest::ConnectionState state =
      connected ? ftest::ConnectionState::kConnected : ftest::ConnectionState::kDisconnected;

  connection_states_.emplace_back(state);
  MaybeUpdateConnectionStates();
}

void EmulatedPeer::MaybeUpdateConnectionStates() {
  std::lock_guard<std::mutex> lock(connection_states_lock_);
  if (connection_states_.empty() || connection_states_completers_.empty()) {
    return;
  }
  while (!connection_states_completers_.empty()) {
    connection_states_completers_.front().Reply(connection_states_);
    connection_states_completers_.pop();
  }
  connection_states_.clear();
}

void EmulatedPeer::OnChannelClosed(fidl::UnbindInfo info) {
  bt_log(TRACE, "virtual", "EmulatedPeer channel closed\n");
  NotifyChannelClosed();
}

void EmulatedPeer::CleanUp() { fake_controller_->RemovePeer(address_); }

void EmulatedPeer::NotifyChannelClosed() {
  if (closed_callback_) {
    closed_callback_();
  }
}

}  // namespace bt_hci_virtual
