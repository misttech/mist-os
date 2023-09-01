/*
 * Copyright (c) 2019 The Fuchsia Authors
 *
 * Permission to use, copy, modify, and/or distribute this software for any
 * purpose with or without fee is hereby granted, provided that the above
 * copyright notice and this permission notice appear in all copies.
 *
 * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
 * WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY
 * SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
 * WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION
 * OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
 * CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
 */

#include <fuchsia/hardware/wlanphyimpl/c/banjo.h>
#include <lib/fdio/directory.h>
#include <zircon/errors.h>

#include <algorithm>
#include <memory>

#include <fbl/string_buffer.h>
#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-device/device.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/common.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/core.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim_device.h"
#include "zircon/system/ulib/sync/include/lib/sync/cpp/completion.h"

namespace wlan {
namespace brcmfmac {
namespace {

std::pair<zx::channel, zx::channel> make_channel() {
  zx::channel local;
  zx::channel remote;
  zx::channel::create(0, &local, &remote);
  return {std::move(local), std::move(remote)};
}

TEST(LifecycleTest, StartStop) {
  auto env = std::make_shared<simulation::Environment>();
  auto dev_mgr = std::make_unique<simulation::FakeDevMgr>();

  SimDevice* device;
  zx_status_t status = SimDevice::Create(dev_mgr->GetRootDevice(), dev_mgr.get(), env, &device);
  ASSERT_EQ(status, ZX_OK);
  status = device->BusInit();
  ASSERT_EQ(status, ZX_OK);
}

TEST(LifecycleTest, StartWithSmeChannel) {
  auto env = std::make_shared<simulation::Environment>();
  auto dev_mgr = std::make_unique<simulation::FakeDevMgr>();
  zx_status_t status;

  // For dispatcher shutdown.
  libsync::Completion completion_;

  auto dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, __func__,
      [&](fdf_dispatcher_t*) { completion_.Signal(); });

  ASSERT_FALSE(dispatcher.is_error());
  auto driver_dispatcher = *std::move(dispatcher);

  // Create PHY.
  SimDevice* device;
  libsync::Completion device_created;
  async::PostTask(driver_dispatcher.async_dispatcher(), [&]() {
    status = SimDevice::Create(dev_mgr->GetRootDevice(), dev_mgr.get(), env, &device);
    ASSERT_EQ(status, ZX_OK);
    device_created.Signal();
  });
  device_created.Wait();

  status = device->BusInit();
  ASSERT_EQ(status, ZX_OK);
  EXPECT_EQ(dev_mgr->DeviceCountByProtocolId(ZX_PROTOCOL_WLANPHY_IMPL), 1u);

  // Connect to device.
  auto outgoing_dir_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  EXPECT_FALSE(outgoing_dir_endpoints.is_error());

  libsync::Completion served;
  async::PostTask(driver_dispatcher.async_dispatcher(), [&]() {
    ASSERT_EQ(ZX_OK, device->ServeWlanPhyImplProtocol(std::move(outgoing_dir_endpoints->server)));
    served.Signal();
  });
  served.Wait();

  auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_phyimpl::Service::WlanPhyImpl::ProtocolType>();
  EXPECT_FALSE(endpoints.is_error());
  zx::channel client_token, server_token;
  EXPECT_EQ(ZX_OK, zx::channel::create(0, &client_token, &server_token));
  EXPECT_EQ(ZX_OK, fdf::ProtocolConnect(std::move(client_token),
                                        fdf::Channel(endpoints->server.TakeChannel().release())));
  fbl::StringBuffer<fuchsia_io::wire::kMaxPathLength> path;
  path.AppendPrintf("svc/%s/default/%s", fuchsia_wlan_phyimpl::Service::WlanPhyImpl::ServiceName,
                    fuchsia_wlan_phyimpl::Service::WlanPhyImpl::Name);
  EXPECT_EQ(ZX_OK, fdio_service_connect_at(outgoing_dir_endpoints->client.channel().get(),
                                           path.c_str(), server_token.release()));

  auto client =
      fdf::WireSyncClient<fuchsia_wlan_phyimpl::WlanPhyImpl>(std::move(endpoints->client));

  constexpr uint32_t kTag = 'TEST';
  fdf::Arena test_arena_(kTag);

  // Create iface.
  auto [local, _remote] = make_channel();
  zx_handle_t test_mlme_channel = local.get();

  // check that create iface fails when we don't provide role and mlme_channel
  fidl::Arena create_fail_arena;
  auto builder_create_fail =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceRequest::Builder(create_fail_arena);
  auto result_create_fail = client.buffer(test_arena_)->CreateIface(builder_create_fail.Build());
  EXPECT_TRUE(result_create_fail.ok());
  ASSERT_TRUE(result_create_fail->is_error());
  ASSERT_EQ(result_create_fail->error_value(), ZX_ERR_INVALID_ARGS);

  // create iface successfully
  fidl::Arena create_arena;
  auto builder_create =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceRequest::Builder(create_arena);
  builder_create.role(wlan_common::WlanMacRole::kClient);
  builder_create.mlme_channel(std::move(local));
  auto result_create = client.buffer(test_arena_)->CreateIface(builder_create.Build());
  EXPECT_TRUE(result_create.ok());
  ASSERT_FALSE(result_create->is_error());
  uint16_t iface_id = result_create->value()->iface_id();
  EXPECT_EQ(dev_mgr->DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);

  // check that creating another client interface fails with ZX_ERR_NO_RESOURCES
  auto [local_again, _remote_again] = make_channel();
  fidl::Arena create_arena_again;
  auto builder_create_again =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceRequest::Builder(create_arena_again);
  builder_create_again.role(wlan_common::WlanMacRole::kClient);
  builder_create_again.mlme_channel(std::move(local_again));
  auto result_create_again = client.buffer(test_arena_)->CreateIface(builder_create_again.Build());
  EXPECT_TRUE(result_create_again.ok());
  ASSERT_TRUE(result_create_again->is_error());
  ASSERT_EQ(result_create_again->error_value(), ZX_ERR_NO_RESOURCES);

  // Simulate start call from Fuchsia's generic wlanif-impl driver.
  auto iface = dev_mgr->FindLatestByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL);
  ASSERT_NE(iface, nullptr);

  auto fullmac_endpoints =
      fdf::CreateEndpoints<fuchsia_wlan_fullmac::Service::WlanFullmacImpl::ProtocolType>();
  EXPECT_FALSE(fullmac_endpoints.is_error());
  {
    zx::channel client_token, server_token;
    EXPECT_EQ(ZX_OK, zx::channel::create(0, &client_token, &server_token));
    EXPECT_EQ(ZX_OK, fdf::ProtocolConnect(
                         std::move(client_token),
                         fdf::Channel(fullmac_endpoints->server.TakeChannel().release())));

    fbl::StringBuffer<fuchsia_io::wire::kMaxPathLength> path;
    path.AppendPrintf("svc/%s/default/%s",
                      fuchsia_wlan_fullmac::Service::WlanFullmacImpl::ServiceName,
                      fuchsia_wlan_fullmac::Service::WlanFullmacImpl::Name);
    // Serve the WlanFullmacImpl protocol on `server_token` found at `path` within
    // the outgoing directory. Here we get the client end outgoing_dir_channel from the device
    // managed by FakeDevMgr.
    EXPECT_EQ(ZX_OK, status = fdio_service_connect_at(iface->DevArgs().outgoing_dir_channel,
                                                      path.c_str(), server_token.release()));
  }
  auto fullmac_client = fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImpl>(
      std::move(fullmac_endpoints->client));

  fdf::Arena arena('T');
  {
    auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfc>();
    EXPECT_FALSE(endpoints.is_error());

    auto result = fullmac_client.buffer(arena)->Start(std::move(endpoints->client));
    EXPECT_TRUE(result.ok());
    EXPECT_FALSE(result->is_error());
    EXPECT_EQ(result->value()->sme_channel.get(), test_mlme_channel);
  }

  {
    // Verify calling start again will fail with proper error code.
    auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfc>();
    EXPECT_FALSE(endpoints.is_error());
    auto result = fullmac_client.buffer(arena)->Start(std::move(endpoints->client));
    EXPECT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
    EXPECT_EQ(result->error_value(), ZX_ERR_ALREADY_BOUND);
  }

  // check that without specifying iface id, destroy iface fails
  fidl::Arena destroy_fail_arena;
  auto builder_destroy_fail =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplDestroyIfaceRequest::Builder(destroy_fail_arena);
  auto result_destroy_fail = client.buffer(test_arena_)->DestroyIface(builder_destroy_fail.Build());
  EXPECT_TRUE(result_destroy_fail.ok());
  ASSERT_TRUE(result_destroy_fail->is_error());
  ASSERT_EQ(result_destroy_fail->error_value(), ZX_ERR_INVALID_ARGS);

  // Now actually destroy the iface
  fidl::Arena destroy_arena;
  auto builder_destroy =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplDestroyIfaceRequest::Builder(destroy_arena);
  builder_destroy.iface_id(iface_id);
  auto result_destroy = client.buffer(test_arena_)->DestroyIface(builder_destroy.Build());
  EXPECT_TRUE(result_destroy.ok());
  ASSERT_FALSE(result_destroy->is_error());
  EXPECT_EQ(dev_mgr->DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 0u);

  libsync::Completion host_destroyed;
  async::PostTask(driver_dispatcher.async_dispatcher(), [&]() {
    dev_mgr.reset();
    host_destroyed.Signal();
  });
  host_destroyed.Wait();
  driver_dispatcher.ShutdownAsync();
  completion_.Wait();
}

}  // namespace
}  // namespace brcmfmac
}  // namespace wlan
