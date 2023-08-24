// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/network/mdns/service/transport/mdns_transceiver.h"

#include <fuchsia/hardware/network/cpp/fidl.h>
#include <fuchsia/net/interfaces/cpp/fidl.h>
#include <fuchsia/net/interfaces/cpp/fidl_test_base.h>
#include <lib/fidl/cpp/binding.h>

#include "src/connectivity/network/mdns/service/common/mdns_addresses.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/lib/testing/predicates/status.h"

namespace mdns::test {

constexpr std::array<uint8_t, 4> kIPv4Address1 = {1, 2, 3, 1};
constexpr std::array<uint8_t, 4> kIPv4Address2 = {4, 5, 6, 1};
constexpr std::array<uint8_t, 16> kIPv6Address1 = {0xfe, 0x80, 0, 0, 0, 0, 0, 0,
                                                   0,    0,    0, 0, 1, 2, 3, 4};
constexpr std::array<uint8_t, 16> kIPv6Address2 = {0xfe, 0x80, 0, 0, 0, 0, 0, 0,
                                                   0,    0,    0, 0, 4, 3, 2, 1};
constexpr std::array<uint8_t, 16> kIPv6AddressNotLinkLocal = {0xff, 0x80, 0, 0, 0, 0, 0, 0,
                                                              0,    0,    0, 0, 4, 3, 2, 1};
constexpr std::array<uint8_t, 16> kIPv6AddressNotLinkLocal2 = {0xff, 0x80, 0, 0, 0, 0, 0, 0,
                                                               0,    0,    0, 0, 4, 3, 2, 2};
constexpr std::array<uint8_t, 16> kIPv6AddressNotLinkLocal3 = {0xff, 0x80, 0, 0, 0, 0, 0, 0,
                                                               0,    0,    0, 0, 4, 3, 2, 3};
constexpr std::array<uint8_t, 16> kIPv6AddressNotLinkLocal4 = {0xff, 0x80, 0, 0, 0, 0, 0, 0,
                                                               0,    0,    0, 0, 4, 3, 2, 4};
constexpr uint8_t kIPv4PrefixLength = 24;
constexpr uint8_t kIPv6PrefixLength = 64;
constexpr uint8_t kID = 1;
constexpr const char kName[] = "test01";
constexpr const char kHostName[] = "testhostname";

namespace {

fuchsia::net::Ipv4Address ToFIDL(const std::array<uint8_t, 4>& bytes) {
  fuchsia::net::Ipv4Address addr;
  std::copy(bytes.cbegin(), bytes.cend(), addr.addr.begin());
  return addr;
}

fuchsia::net::Ipv6Address ToFIDL(const std::array<uint8_t, 16>& bytes) {
  fuchsia::net::Ipv6Address addr;
  std::copy(bytes.cbegin(), bytes.cend(), addr.addr.begin());
  return addr;
}

void InitAddress(fuchsia::net::interfaces::Address& addr, const std::array<uint8_t, 4>& bytes) {
  fuchsia::net::Subnet subnet{
      .prefix_len = kIPv4PrefixLength,
  };
  subnet.addr.set_ipv4(ToFIDL(bytes));
  addr.set_addr(std::move(subnet));
}

void InitAddress(fuchsia::net::interfaces::Address& addr, const std::array<uint8_t, 16>& bytes) {
  fuchsia::net::Subnet subnet{
      .prefix_len = kIPv6PrefixLength,
  };
  subnet.addr.set_ipv6(ToFIDL(bytes));
  addr.set_addr(std::move(subnet));
}

std::vector<fuchsia::net::interfaces::Address> Addresses1() {
  std::vector<fuchsia::net::interfaces::Address> addresses;
  addresses.reserve(2);
  InitAddress(addresses.emplace_back(), kIPv4Address1);
  InitAddress(addresses.emplace_back(), kIPv6Address1);
  // To verify that non-link-local addresses do not cause a transceiver to be created.
  InitAddress(addresses.emplace_back(), kIPv6AddressNotLinkLocal);
  return addresses;
}

std::vector<fuchsia::net::interfaces::Address> Addresses1Extra() {
  std::vector<fuchsia::net::interfaces::Address> addresses;
  addresses.reserve(2);
  InitAddress(addresses.emplace_back(), kIPv4Address1);
  InitAddress(addresses.emplace_back(), kIPv6Address1);
  InitAddress(addresses.emplace_back(), kIPv6AddressNotLinkLocal);
  InitAddress(addresses.emplace_back(), kIPv6AddressNotLinkLocal2);
  InitAddress(addresses.emplace_back(), kIPv6AddressNotLinkLocal3);
  InitAddress(addresses.emplace_back(), kIPv6AddressNotLinkLocal4);
  return addresses;
}

std::vector<fuchsia::net::interfaces::Address> Addresses2() {
  std::vector<fuchsia::net::interfaces::Address> addresses;
  addresses.reserve(2);
  InitAddress(addresses.emplace_back(), kIPv4Address2);
  InitAddress(addresses.emplace_back(), kIPv6Address2);
  return addresses;
}

bool VerifyAddressResource(std::shared_ptr<DnsResource> resource, inet::IpAddress expected_address,
                           bool cache_flush) {
  EXPECT_TRUE(!!resource);
  if (!resource) {
    return false;
  }

  EXPECT_EQ(kHostName, resource->name_.dotted_string_);
  EXPECT_EQ(expected_address.is_v4() ? DnsType::kA : DnsType::kAaaa, resource->type_);
  EXPECT_EQ(DnsClass::kIn, resource->class_);
  EXPECT_EQ(cache_flush, resource->cache_flush_);
  EXPECT_EQ(DnsResource::kShortTimeToLive, resource->time_to_live_);

  if (resource->type_ == DnsType::kA) {
    EXPECT_EQ(expected_address, resource->a_.address_.address_);
    if (expected_address != resource->a_.address_.address_) {
      return false;
    }
  } else {
    EXPECT_EQ(expected_address, resource->aaaa_.address_.address_);
    if (expected_address != resource->aaaa_.address_.address_) {
      return false;
    }
  }

  return (kHostName == resource->name_.dotted_string_) &&
         ((expected_address.is_v4() ? DnsType::kA : DnsType::kAaaa) == resource->type_) &&
         (DnsClass::kIn == resource->class_) && (cache_flush == resource->cache_flush_) &&
         (DnsResource::kShortTimeToLive == resource->time_to_live_);
}

}  // namespace

class InterfacesWatcherImpl : public fuchsia::net::interfaces::testing::Watcher_TestBase {
 public:
  void NotImplemented_(const std::string& name) override {
    std::cout << "Not implemented: " << name << std::endl;
  }

  void Watch(WatchCallback callback) override {
    ASSERT_FALSE(watch_cb_.has_value());
    watch_cb_ = std::move(callback);
  }

  bool CompleteWatchCallback(fuchsia::net::interfaces::Event event) {
    if (watch_cb_.has_value()) {
      (*watch_cb_)(std::move(event));
      watch_cb_.reset();
      return true;
    } else {
      return false;
    }
  }

 private:
  std::optional<WatchCallback> watch_cb_;
};

class StubInterfaceTransceiver : public MdnsInterfaceTransceiver {
 public:
  StubInterfaceTransceiver(inet::IpAddress address, const std::string& name, uint32_t id,
                           Media media)
      : MdnsInterfaceTransceiver(address, name, id, media),
        ip_versions_(address.is_v4() ? IpVersions::kV4 : IpVersions::kV6) {}

  static std::unique_ptr<MdnsInterfaceTransceiver> Create(inet::IpAddress address,
                                                          const std::string& name, uint32_t id,
                                                          Media media) {
    return std::make_unique<StubInterfaceTransceiver>(address, name, id, media);
  }

  const std::vector<std::shared_ptr<DnsResource>>& GetInterfaceAddressResources(
      const std::string& host_full_name) {
    return MdnsInterfaceTransceiver::GetInterfaceAddressResources(host_full_name);
  }

 protected:
  enum IpVersions IpVersions() override { return ip_versions_; }
  int SetOptionDisableMulticastLoop() override { return 0; }
  int SetOptionJoinMulticastGroup() override { return 0; }
  int SetOptionOutboundInterface() override { return 0; }
  int SetOptionUnicastTtl() override { return 0; }
  int SetOptionMulticastTtl() override { return 0; }
  int SetOptionFamilySpecific() override { return 0; }
  int Bind() override { return 0; }
  ssize_t SendTo(const void* buffer, size_t size, const inet::SocketAddress& address) override {
    return 0;
  }

  bool Start(InboundMessageCallback callback) override { return true; }
  void Stop() override {}

 private:
  enum IpVersions ip_versions_;
};

class MdnsTransceiverTests : public gtest::TestLoopFixture {
 public:
  MdnsTransceiverTests()
      : binding_(std::make_unique<fidl::Binding<fuchsia::net::interfaces::Watcher>>(
            &fake_watcher_impl_)),
        v4_address1_(inet::IpAddress(ToFIDL(kIPv4Address1))),
        v4_address2_(inet::IpAddress(ToFIDL(kIPv4Address2))),
        v6_address1_(inet::IpAddress(ToFIDL(kIPv6Address1))),
        v6_address2_(inet::IpAddress(ToFIDL(kIPv6Address2))),
        v6_address_not_link_local_(inet::IpAddress(ToFIDL(kIPv6AddressNotLinkLocal))),
        v6_address_not_link_local_2_(inet::IpAddress(ToFIDL(kIPv6AddressNotLinkLocal2))),
        v6_address_not_link_local_3_(inet::IpAddress(ToFIDL(kIPv6AddressNotLinkLocal3))),
        v6_address_not_link_local_4_(inet::IpAddress(ToFIDL(kIPv6AddressNotLinkLocal4))) {
    properties_ = fuchsia::net::interfaces::Properties();
    properties_.set_id(kID);
    properties_.set_name(kName);
    properties_.set_device_class(fuchsia::net::interfaces::DeviceClass::WithDevice(
        fuchsia::hardware::network::DeviceClass::WLAN));
    properties_.set_online(true);
    properties_.set_has_default_ipv4_route(false);
    properties_.set_has_default_ipv6_route(false);

    properties_.set_addresses(Addresses1());
  }

 protected:
  void SetUp() override {
    TestLoopFixture::SetUp();

    fuchsia::net::interfaces::WatcherPtr watcher;
    ASSERT_OK(binding_->Bind(watcher.NewRequest()));
    transceiver_.Start(
        std::move(watcher), []() {}, [](auto, auto) {}, StubInterfaceTransceiver::Create);
  }

  void TearDown() override {
    transceiver_.Stop();

    TestLoopFixture::TearDown();
  }

  MdnsTransceiver transceiver_;
  InterfacesWatcherImpl fake_watcher_impl_;
  const std::unique_ptr<fidl::Binding<fuchsia::net::interfaces::Watcher>> binding_;
  fuchsia::net::interfaces::Properties properties_;
  const inet::IpAddress v4_address1_, v4_address2_, v6_address1_, v6_address2_,
      v6_address_not_link_local_, v6_address_not_link_local_2_, v6_address_not_link_local_3_,
      v6_address_not_link_local_4_;
};

TEST_F(MdnsTransceiverTests, IgnoreLoopback) {
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);

  properties_.set_device_class(
      fuchsia::net::interfaces::DeviceClass::WithLoopback(fuchsia::net::interfaces::Empty()));

  RunLoopUntilIdle();

  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithExisting(std::move(properties_))));

  RunLoopUntilIdle();

  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);
}

TEST_F(MdnsTransceiverTests, OnlineChange) {
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);

  RunLoopUntilIdle();

  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithExisting(std::move(properties_))));

  RunLoopUntilIdle();

  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);

  fuchsia::net::interfaces::Properties online_false;
  online_false.set_id(kID);
  online_false.set_online(false);
  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithChanged(std::move(online_false))));

  RunLoopUntilIdle();

  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);

  fuchsia::net::interfaces::Properties online_true;
  online_false.set_id(kID);
  online_false.set_online(true);
  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithChanged(std::move(online_false))));

  RunLoopUntilIdle();

  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);
}

TEST_F(MdnsTransceiverTests, AddressesChange) {
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);

  RunLoopUntilIdle();

  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithExisting(std::move(properties_))));

  RunLoopUntilIdle();

  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);

  fuchsia::net::interfaces::Properties addresses_change;
  addresses_change.set_id(kID);
  addresses_change.set_addresses(Addresses2());
  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithChanged(std::move(addresses_change))));

  RunLoopUntilIdle();

  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v4_address2_), nullptr);
  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v6_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);
}

TEST_F(MdnsTransceiverTests, OnlineAndAddressesChange) {
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);

  RunLoopUntilIdle();

  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithExisting(std::move(properties_))));

  RunLoopUntilIdle();

  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);

  fuchsia::net::interfaces::Properties offline_and_addresses_change;
  offline_and_addresses_change.set_id(kID);
  offline_and_addresses_change.set_online(false);
  offline_and_addresses_change.set_addresses(Addresses2());

  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithChanged(std::move(offline_and_addresses_change))));

  RunLoopUntilIdle();

  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);

  fuchsia::net::interfaces::Properties online_and_addresses_change;
  online_and_addresses_change.set_id(kID);
  online_and_addresses_change.set_online(true);
  online_and_addresses_change.set_addresses(Addresses1());
  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithChanged(std::move(online_and_addresses_change))));

  RunLoopUntilIdle();

  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v4_address1_), nullptr);
  EXPECT_NE(transceiver_.GetInterfaceTransceiver(v6_address1_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v4_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address2_), nullptr);
  EXPECT_EQ(transceiver_.GetInterfaceTransceiver(v6_address_not_link_local_), nullptr);
}

// Tests that interface address resources at the transceivers are updated properly when address
// changes occur and that cache_flush and other resource properties are properly set.
TEST_F(MdnsTransceiverTests, InterfaceAddressResources) {
  RunLoopUntilIdle();

  // Send the initial NIC configuration. Initial addresses are |v4_address1_|, |v6_address1_| and
  // |v6_address_not_link_local_|.
  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithExisting(std::move(properties_))));

  RunLoopUntilIdle();

  // Get the expected transceivers created based on the NIC configuration.
  auto v4_transceiver = reinterpret_cast<StubInterfaceTransceiver*>(
      transceiver_.GetInterfaceTransceiver(v4_address1_));
  auto v6_transceiver = reinterpret_cast<StubInterfaceTransceiver*>(
      transceiver_.GetInterfaceTransceiver(v6_address1_));

  EXPECT_NE(v4_transceiver, nullptr);
  EXPECT_NE(v6_transceiver, nullptr);

  // Expect that the correct address resources are generated by the transceivers with the
  // correct cache_flush bits. For a given transceiver, the first A (V4) and the first AAAA (V6)
  // resource should have cache_flush set and the remainder of the A/AAAA resources should not.
  auto& v4_addr = v4_transceiver->GetInterfaceAddressResources(kHostName);
  EXPECT_EQ(3u, v4_addr.size());
  EXPECT_TRUE(VerifyAddressResource(v4_addr[0], v4_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v4_addr[1], v6_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v4_addr[2], v6_address_not_link_local_, false));

  auto& v6_addr = v6_transceiver->GetInterfaceAddressResources(kHostName);
  EXPECT_EQ(3u, v6_addr.size());
  EXPECT_TRUE(VerifyAddressResource(v6_addr[0], v4_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v6_addr[1], v6_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v6_addr[2], v6_address_not_link_local_, false));

  // Send an address change that adds new V6 addresses.
  fuchsia::net::interfaces::Properties addresses_change;
  addresses_change.set_id(kID);
  addresses_change.set_addresses(Addresses1Extra());
  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithChanged(std::move(addresses_change))));

  RunLoopUntilIdle();

  // Ensure that the transceivers have not been deleted or replaced.
  EXPECT_EQ(v4_transceiver, reinterpret_cast<StubInterfaceTransceiver*>(
                                transceiver_.GetInterfaceTransceiver(v4_address1_)));
  EXPECT_EQ(v6_transceiver, reinterpret_cast<StubInterfaceTransceiver*>(
                                transceiver_.GetInterfaceTransceiver(v6_address1_)));

  // Expect that the correct address resources are generated by the transceivers with the
  // correct cache_flush bits. For a given transceiver, the first A (V4) and the first AAAA (V6)
  // resource should have cache_flush set and the remainder of the A/AAAA resources should not.
  auto& v4_addr_2 = v4_transceiver->GetInterfaceAddressResources(kHostName);
  EXPECT_EQ(6u, v4_addr.size());
  EXPECT_TRUE(VerifyAddressResource(v4_addr_2[0], v4_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v4_addr_2[1], v6_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v4_addr_2[2], v6_address_not_link_local_, false));
  EXPECT_TRUE(VerifyAddressResource(v4_addr_2[3], v6_address_not_link_local_2_, false));
  EXPECT_TRUE(VerifyAddressResource(v4_addr_2[4], v6_address_not_link_local_3_, false));
  EXPECT_TRUE(VerifyAddressResource(v4_addr_2[5], v6_address_not_link_local_4_, false));

  auto& v6_addr_2 = v6_transceiver->GetInterfaceAddressResources(kHostName);
  EXPECT_EQ(6u, v6_addr_2.size());
  EXPECT_TRUE(VerifyAddressResource(v6_addr_2[0], v4_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v6_addr_2[1], v6_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v6_addr_2[2], v6_address_not_link_local_, false));
  EXPECT_TRUE(VerifyAddressResource(v6_addr_2[3], v6_address_not_link_local_2_, false));
  EXPECT_TRUE(VerifyAddressResource(v6_addr_2[4], v6_address_not_link_local_3_, false));
  EXPECT_TRUE(VerifyAddressResource(v6_addr_2[5], v6_address_not_link_local_4_, false));

  // Send an address change that remove some V6 addresses.
  fuchsia::net::interfaces::Properties addresses_change_2;
  addresses_change_2.set_id(kID);
  addresses_change_2.set_addresses(Addresses1());
  ASSERT_TRUE(fake_watcher_impl_.CompleteWatchCallback(
      fuchsia::net::interfaces::Event::WithChanged(std::move(addresses_change_2))));

  RunLoopUntilIdle();

  // Ensure that the transceivers have not been deleted or replaced.
  EXPECT_EQ(v4_transceiver, reinterpret_cast<StubInterfaceTransceiver*>(
                                transceiver_.GetInterfaceTransceiver(v4_address1_)));
  EXPECT_EQ(v6_transceiver, reinterpret_cast<StubInterfaceTransceiver*>(
                                transceiver_.GetInterfaceTransceiver(v6_address1_)));

  // Expect that the correct address resources are generated by the transceivers with the
  // correct cache_flush bits. For a given transceiver, the first A (V4) and the first AAAA (V6)
  // resource should have cache_flush set and the remainder of the A/AAAA resources should not.
  auto& v4_addr_3 = v4_transceiver->GetInterfaceAddressResources(kHostName);
  EXPECT_EQ(3u, v4_addr_3.size());
  EXPECT_TRUE(VerifyAddressResource(v4_addr_3[0], v4_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v4_addr_3[1], v6_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v4_addr_3[2], v6_address_not_link_local_, false));

  auto& v6_addr_3 = v6_transceiver->GetInterfaceAddressResources(kHostName);
  EXPECT_EQ(3u, v6_addr_3.size());
  EXPECT_TRUE(VerifyAddressResource(v6_addr_3[0], v4_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v6_addr_3[1], v6_address1_, true));
  EXPECT_TRUE(VerifyAddressResource(v6_addr_3[2], v6_address_not_link_local_, false));
}

}  // namespace mdns::test
