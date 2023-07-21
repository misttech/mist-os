// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "network.h"

#include "gmock/gmock.h"
#include "mock_boot_service.h"

namespace gigaboot {
namespace {

class NetworkTest : public ::testing::Test {
 public:
  NetworkTest()
      : image_device_({"path-A", "path-B", "path-C", "image"}),
        net_device_({"path-A", "path-B", "path-C"},
                    {
                        .CurrentAddress = {0xF4, 0x93, 0x9F, 0xF4, 0xF8, 0x24},
                        .MediaPresent = true,
                    }) {
    stub_service_.AddDevice(&image_device_);
    stub_service_.AddDevice(&net_device_);
  }

  MockStubService& stub_service() { return stub_service_; }
  Device& image_device() { return image_device_; }
  ManagedNetworkDevice& net_device() { return net_device_; }

 private:
  MockStubService stub_service_;
  Device image_device_;
  ManagedNetworkDevice net_device_;
};

struct BadIntfTestCase {
  std::string_view name;
  void (*mutate_net_device)(ManagedNetworkDevice&) = [](auto&) {};
};

class NetworkFindIntfFailureTest : public NetworkTest,
                                   public testing::WithParamInterface<BadIntfTestCase> {};

TEST_P(NetworkFindIntfFailureTest, NetIntfCreationTest) {
  auto cleanup = SetupEfiGlobalState(stub_service(), image_device());

  const BadIntfTestCase& test_case = GetParam();
  test_case.mutate_net_device(net_device());

  ASSERT_TRUE(EthernetAgent::Create().is_error());
}

INSTANTIATE_TEST_SUITE_P(
    NetworkFindIntfFailureTests, NetworkFindIntfFailureTest,
    testing::ValuesIn<NetworkFindIntfFailureTest::ParamType>({
        {"get_mode_failure",
         [](ManagedNetworkDevice& d) {
           d.GetManagedNetworkProtocol()->GetModeData = [](auto*, auto*, auto*)
                                                            EFIAPI { return EFI_UNSUPPORTED; };
         }},
        {"configure_stop_failure",
         [](ManagedNetworkDevice& d) {
           d.GetManagedNetworkProtocol()->Configure = [](auto*, auto*)
                                                          EFIAPI { return EFI_UNSUPPORTED; };
         }},
        {"no_media_present",
         [](ManagedNetworkDevice& d) { d.SetModeData({.MediaPresent = false}); }},
        {"configure_enable_failure",
         [](ManagedNetworkDevice& d) {
           d.GetManagedNetworkProtocol()->Configure = [](auto*, auto* data) EFIAPI {
             return data == nullptr ? EFI_SUCCESS : EFI_UNSUPPORTED;
           };
         }},
    }),
    [](testing::TestParamInfo<NetworkFindIntfFailureTest::ParamType> const& info) {
      return std::string(info.param.name.begin(), info.param.name.end());
    });

TEST_F(NetworkTest, CreateAgentAndTx) {
  auto cleanup = SetupEfiGlobalState(stub_service(), image_device());
  auto res = EthernetAgent::Create();
  ASSERT_TRUE(res.is_ok());
  EthernetAgent agent = std::move(res.value());

  MacAddr dest = {0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA};
  uint8_t payload[] = {0x11, 0x22, 0x33, 0x44, 0x55};
  efi_handle callback = reinterpret_cast<efi_handle>(0xDEADBEEF);
  efi_managed_network_sync_completion_token token;

  ASSERT_TRUE(agent
                  .SendV6Frame(dest, cpp20::span<const uint8_t>(payload, std::size(payload)),
                               callback, &token)
                  .is_ok());

  // This is a fragile, quick and dirty check to make sure that
  // _something_ gets transmitted and that
  // 1) it has a valid, expected ethernet header
  // 2) it has the payload contents that we tried to transmit
  //
  // This is very much not what a real transmitted packet would look like
  // except for the ethernet header.
  std::vector<uint8_t> expected;
  MacAddr source = agent.Addr();
  expected.insert(expected.end(), dest.cbegin(), dest.cend());
  expected.insert(expected.end(), source.cbegin(), source.cend());
  expected.push_back(0x86);
  expected.push_back(0xDD);
  expected.insert(expected.end(), std::cbegin(payload), std::cend(payload));

  ASSERT_EQ(expected, net_device().GetFakeProtocol().GetMostRecentTx());
}

TEST_F(NetworkTest, CreateAgentNoConfigure) {
  auto cleanup = SetupEfiGlobalState(stub_service(), image_device());
  net_device().GetManagedNetworkProtocol()->Configure = [](auto*, auto*)
                                                            EFIAPI { return EFI_UNSUPPORTED; };
  net_device().GetFakeProtocol().SetConfigData({.EnableUnicastReceive = true});

  auto res = EthernetAgent::Create();
  ASSERT_TRUE(res.is_ok());
}

}  // namespace
}  // namespace gigaboot
