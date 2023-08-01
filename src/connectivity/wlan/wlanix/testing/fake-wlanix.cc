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

}  // namespace wlanix_test
