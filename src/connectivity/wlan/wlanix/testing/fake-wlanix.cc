// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fake-wlanix.h"

#include "fidl/fuchsia.wlan.wlanix/cpp/wire_types.h"
#include "lib/fidl/cpp/wire/vector_view.h"

namespace wlanix_test {

void FakeWlanix::Connect(async_dispatcher_t* dispatcher,
                         fidl::ServerEnd<fuchsia_wlan_wlanix::Wlanix> server_end) {
  dispatcher_ = dispatcher;
  fidl::BindServer(dispatcher, std::move(server_end), this);
}

void FakeWlanix::GetWifi(fuchsia_wlan_wlanix::wire::WlanixGetWifiRequest* request,
                         GetWifiCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWlanixGetWifi});
  if (!request->has_wifi()) {
    ZX_ASSERT_MSG(false, "expect `wifi` to be present");
  }
  fidl::BindServer(dispatcher_, std::move(request->wifi()), this);
}

void FakeWlanix::GetSupplicant(fuchsia_wlan_wlanix::wire::WlanixGetSupplicantRequest* request,
                               GetSupplicantCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWlanixGetSupplicant});
  if (!request->has_supplicant()) {
    ZX_ASSERT_MSG(false, "expect `supplicant` to be present");
  }
  fidl::BindServer(dispatcher_, std::move(request->supplicant()), this);
}

void FakeWlanix::GetNl80211(fuchsia_wlan_wlanix::wire::WlanixGetNl80211Request* request,
                            GetNl80211Completer::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWlanixGetNl80211});
  ZX_ASSERT_MSG(false, "GetNl80211 is not supported by fake-wlanix");
}

void FakeWlanix::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_wlan_wlanix::Wlanix> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWlanixUnknownMethod});
}

void FakeWlanix::RegisterEventCallback(
    fuchsia_wlan_wlanix::wire::WifiRegisterEventCallbackRequest* request,
    RegisterEventCallbackCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiRegisterEventCallback});
}

void FakeWlanix::Start(StartCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiStart});
  completer.ReplySuccess();
}

void FakeWlanix::Stop(StopCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiStop});
  completer.ReplySuccess();
}

void FakeWlanix::GetState(GetStateCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiGetState});
  fidl::Arena arena;
  auto builder = fuchsia_wlan_wlanix::wire::WifiGetStateResponse::Builder(arena);
  builder.is_started(true);
  completer.Reply(builder.Build());
}

void FakeWlanix::GetChipIds(GetChipIdsCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiGetChipIds});
  fidl::Arena arena;
  auto builder = fuchsia_wlan_wlanix::wire::WifiGetChipIdsResponse::Builder(arena);
  std::vector<uint32_t> chip_ids = {1};
  builder.chip_ids(fidl::VectorView<uint32_t>::FromExternal(chip_ids));
  completer.Reply(builder.Build());
}

void FakeWlanix::GetChip(fuchsia_wlan_wlanix::wire::WifiGetChipRequest* request,
                         GetChipCompleter::Sync& completer) {
  if (!request->has_chip_id() || !request->has_chip()) {
    ZX_ASSERT_MSG(false, "expect `chip id` and `chip` to be present");
  }

  AppendCommand(Command{.tag = CommandTag::kWifiGetChip,
                        .args = {
                            .wifi_get_chip_args =
                                {
                                    .chip_id = request->chip_id(),
                                },
                        }});
  fidl::BindServer(dispatcher_, std::move(request->chip()), this);
  completer.ReplySuccess();
}

void FakeWlanix::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_wlan_wlanix::Wifi> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiUnknownMethod});
}

void FakeWlanix::CreateStaIface(fuchsia_wlan_wlanix::wire::WifiChipCreateStaIfaceRequest* request,
                                CreateStaIfaceCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiChipCreateStaIface});
  if (!request->has_iface()) {
    ZX_ASSERT_MSG(false, "expect `iface` to be present");
  }
  fidl::BindServer(dispatcher_, std::move(request->iface()), this);
  completer.ReplySuccess();
}

void FakeWlanix::GetStaIfaceNames(GetStaIfaceNamesCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiChipGetStaIfaceNames});
  fidl::Arena arena;
  auto builder = fuchsia_wlan_wlanix::wire::WifiChipGetStaIfaceNamesResponse::Builder(arena);
  std::vector<fidl::StringView> iface_names = {};
  builder.iface_names(fidl::VectorView<fidl::StringView>::FromExternal(iface_names));
  completer.Reply(builder.Build());
}

void FakeWlanix::GetStaIface(fuchsia_wlan_wlanix::wire::WifiChipGetStaIfaceRequest* request,
                             GetStaIfaceCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiChipGetStaIface});
  if (!request->has_iface_name() || !request->has_iface()) {
    ZX_ASSERT_MSG(false, "expect `iface_name` and `iface` to be present");
  }
  fidl::BindServer(dispatcher_, std::move(request->iface()), this);
  completer.ReplySuccess();
}

void FakeWlanix::RemoveStaIface(fuchsia_wlan_wlanix::wire::WifiChipRemoveStaIfaceRequest* request,
                                RemoveStaIfaceCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiChipRemoveStaIface});
  if (!request->has_iface_name()) {
    ZX_ASSERT_MSG(false, "expect `iface_name` to be present");
  }
  completer.ReplySuccess();
}

void FakeWlanix::SetCountryCode(fuchsia_wlan_wlanix::wire::WifiChipSetCountryCodeRequest* request,
                                SetCountryCodeCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiChipSetCountryCode});
  if (!request->has_code()) {
    ZX_ASSERT_MSG(false, "expect `code` to be present");
  }
  completer.ReplySuccess();
}

void FakeWlanix::GetAvailableModes(GetAvailableModesCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiChipGetAvailableModes});

  using IfaceConcurrencyType = fuchsia_wlan_wlanix::wire::IfaceConcurrencyType;
  using ChipConcurrencyCombinationLimit =
      fuchsia_wlan_wlanix::wire::ChipConcurrencyCombinationLimit;
  using ChipConcurrencyCombination = fuchsia_wlan_wlanix::wire::ChipConcurrencyCombination;
  using ChipMode = fuchsia_wlan_wlanix::wire::ChipMode;

  std::vector<IfaceConcurrencyType> types{IfaceConcurrencyType::kSta};
  fidl::Arena arena;
  auto limit_builder = ChipConcurrencyCombinationLimit::Builder(arena);
  limit_builder.types(fidl::VectorView<IfaceConcurrencyType>::FromExternal(types));
  limit_builder.max_ifaces(1);
  std::vector<ChipConcurrencyCombinationLimit> combination_limits{limit_builder.Build()};

  auto combination_builder = ChipConcurrencyCombination::Builder(arena);
  combination_builder.limits(combination_limits);
  std::vector<ChipConcurrencyCombination> combinations{combination_builder.Build()};

  auto chip_mode_builder = ChipMode::Builder(arena);
  chip_mode_builder.id(0);
  chip_mode_builder.available_combinations(
      fidl::VectorView<ChipConcurrencyCombination>::FromExternal(combinations));
  std::vector<ChipMode> chip_modes{chip_mode_builder.Build()};

  auto response_builder =
      fuchsia_wlan_wlanix::wire::WifiChipGetAvailableModesResponse::Builder(arena);
  response_builder.chip_modes(fidl::VectorView<ChipMode>::FromExternal(chip_modes));
  completer.Reply(response_builder.Build());
}

void FakeWlanix::GetId(GetIdCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiChipGetId});
  fidl::Arena arena;
  auto builder = fuchsia_wlan_wlanix::wire::WifiChipGetIdResponse::Builder(arena);
  builder.id(1);
  completer.Reply(builder.Build());
}

void FakeWlanix::GetMode(GetModeCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiChipGetMode});
  fidl::Arena arena;
  auto builder = fuchsia_wlan_wlanix::wire::WifiChipGetModeResponse::Builder(arena);
  builder.mode(0);
  completer.Reply(builder.Build());
}

void FakeWlanix::GetCapabilities(GetCapabilitiesCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiChipGetCapabilities});
  fidl::Arena arena;
  auto builder = fuchsia_wlan_wlanix::wire::WifiChipGetCapabilitiesResponse::Builder(arena);
  builder.capabilities_mask(0);
  completer.Reply(builder.Build());
}

void FakeWlanix::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_wlan_wlanix::WifiChip> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiChipUnknownMethod});
}

void FakeWlanix::GetName(GetNameCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiStaIfaceGetName});
  fidl::Arena arena;
  auto builder = fuchsia_wlan_wlanix::wire::WifiStaIfaceGetNameResponse::Builder(arena);
  builder.iface_name(fidl::StringView("sta-iface"));
  completer.Reply(builder.Build());
}

void FakeWlanix::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_wlan_wlanix::WifiStaIface> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kWifiStaIfaceUnknownMethod});
}

void FakeWlanix::AddStaInterface(
    fuchsia_wlan_wlanix::wire::SupplicantAddStaInterfaceRequest* request,
    AddStaInterfaceCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantAddStaInterface});
  if (!request->has_iface()) {
    ZX_ASSERT_MSG(false, "expect `iface` to be present");
  }
  fidl::BindServer(dispatcher_, std::move(request->iface()), this);
}

void FakeWlanix::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_wlan_wlanix::Supplicant> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantUnknownMethod});
}

void FakeWlanix::RegisterCallback(
    fuchsia_wlan_wlanix::wire::SupplicantStaIfaceRegisterCallbackRequest* request,
    RegisterCallbackCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaIfaceRegisterCallback});
}

void FakeWlanix::AddNetwork(fuchsia_wlan_wlanix::wire::SupplicantStaIfaceAddNetworkRequest* request,
                            AddNetworkCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaIfaceAddNetwork});
  if (!request->has_network()) {
    ZX_ASSERT_MSG(false, "expect `network` to be present");
  }
  fidl::BindServer(dispatcher_, std::move(request->network()), this);
}

void FakeWlanix::Disconnect(DisconnectCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaIfaceDisconnect});
}

void FakeWlanix::SetPowerSave(
    fuchsia_wlan_wlanix::wire::SupplicantStaIfaceSetPowerSaveRequest* request,
    SetPowerSaveCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaIfaceSetPowerSave});
  if (!request->has_enable()) {
    ZX_ASSERT_MSG(false, "expect `enable` to be present");
  }
}

void FakeWlanix::SetSuspendModeEnabled(
    fuchsia_wlan_wlanix::wire::SupplicantStaIfaceSetSuspendModeEnabledRequest* request,
    SetSuspendModeEnabledCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaIfaceSetSuspendModeEnabled});
  if (!request->has_enable()) {
    ZX_ASSERT_MSG(false, "expect `enable` to be present");
  }
}

void FakeWlanix::SetStaCountryCode(
    fuchsia_wlan_wlanix::wire::SupplicantStaIfaceSetStaCountryCodeRequest* request,
    SetStaCountryCodeCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaIfaceSetStaCountryCode});
  if (!request->has_code()) {
    ZX_ASSERT_MSG(false, "expect `code` to be present");
  }
}

void FakeWlanix::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_wlan_wlanix::SupplicantStaIface> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaIfaceUnknownMethod});
}

void FakeWlanix::SetBssid(fuchsia_wlan_wlanix::wire::SupplicantStaNetworkSetBssidRequest* request,
                          SetBssidCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaNetworkSetBssid});
}

void FakeWlanix::ClearBssid(ClearBssidCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaNetworkClearBssid});
}

void FakeWlanix::SetSsid(fuchsia_wlan_wlanix::wire::SupplicantStaNetworkSetSsidRequest* request,
                         SetSsidCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaNetworkSetSsid});
}

void FakeWlanix::SetPskPassphrase(
    fuchsia_wlan_wlanix::wire::SupplicantStaNetworkSetPskPassphraseRequest* request,
    SetPskPassphraseCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaNetworkSetPskPassphrase});
}

void FakeWlanix::Select(SelectCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaNetworkSelect});
}

void FakeWlanix::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_wlan_wlanix::SupplicantStaNetwork> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  AppendCommand(Command{.tag = CommandTag::kSupplicantStaNetworkUnknownMethod});
}

}  // namespace wlanix_test
