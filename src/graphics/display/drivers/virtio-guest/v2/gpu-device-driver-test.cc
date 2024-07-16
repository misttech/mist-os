// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v2/gpu-device-driver.h"

#include <fidl/fuchsia.hardware.pci/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire_test_base.h>
#include <fidl/fuchsia.sysmem2/cpp/wire_test_base.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdf/env.h>
#include <lib/fdf/testing.h>
#include <lib/syslog/cpp/macros.h>

#include <fbl/auto_lock.h>
#include <gtest/gtest.h>

#include "src/devices/testing/fake-bti/include/lib/fake-bti/bti.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/lib/testing/predicates/status.h"

namespace {

// The GPU Virtual Device ID is 0x1040 + 16 = 0x1050, in accordance with
// Section 4.1.2.1 and Section 5 of the VIRTIO V1.2 Spec:
// https://docs.oasis-open.org/virtio/virtio/v1.2/csd01/virtio-v1.2-csd01.html#x1-2160005
// This also matches the definition at
// //zircon/system/ulib/virtio/include/virtio/virtio.h
constexpr int kTestDeviceId = 0x1050;

// The PCI Subsystem Vendor ID according to the same spec linked above.
constexpr int kTestSubsysDeviceId = 0x1af4;

// Use a stub buffer collection instead of the real sysmem since some tests may
// require things that aren't available on the current system.
//
// TODO(https://fxbug.dev/42072949): Consider creating and using a unified set of sysmem
// testing doubles instead of writing mocks for each display driver test.
class StubBufferCollection : public fidl::testing::WireTestBase<fuchsia_sysmem2::BufferCollection> {
 public:
  void SetConstraints(SetConstraintsRequestView request,
                      SetConstraintsCompleter::Sync& _completer) override {
    if (!request->has_constraints()) {
      return;
    }
    auto& image_constraints = request->constraints().image_format_constraints()[0];
    EXPECT_EQ(fuchsia_images2::wire::PixelFormat::kB8G8R8A8, image_constraints.pixel_format());
    EXPECT_EQ(4u, image_constraints.bytes_per_row_divisor());
  }

  void CheckAllBuffersAllocated(CheckAllBuffersAllocatedCompleter::Sync& completer) override {
    completer.Reply(zx::ok());
  }

  void WaitForAllBuffersAllocated(WaitForAllBuffersAllocatedCompleter::Sync& _completer) override {
    auto info = fuchsia_sysmem2::wire::BufferCollectionInfo::Builder(arena_);
    info.settings(
        fuchsia_sysmem2::wire::SingleBufferSettings::Builder(arena_)
            .image_format_constraints(
                fuchsia_sysmem2::wire::ImageFormatConstraints::Builder(arena_)
                    .pixel_format(fuchsia_images2::wire::PixelFormat::kB8G8R8A8)
                    .pixel_format_modifier(fuchsia_images2::wire::PixelFormatModifier::kLinear)
                    .max_size(fuchsia_math::wire::SizeU{
                        .width = 1000, .height = std::numeric_limits<uint32_t>::max()})
                    .max_bytes_per_row(4000)
                    .Build())
            .Build());

    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(4096, 0, &vmo));
    info.buffers(std::vector{fuchsia_sysmem2::wire::VmoBuffer::Builder(arena_)
                                 .vmo(std::move(vmo))
                                 .vmo_usable_start(0)
                                 .Build()});
    auto response =
        fuchsia_sysmem2::wire::BufferCollectionWaitForAllBuffersAllocatedResponse::Builder(arena_)
            .buffer_collection_info(info.Build())
            .Build();
    _completer.Reply(fit::ok(&response));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    EXPECT_TRUE(false);
  }

 private:
  fidl::Arena<fidl::kDefaultArenaInitialCapacity> arena_;
};

class MockAllocator : public fidl::testing::WireTestBase<fuchsia_sysmem2::Allocator> {
 public:
  explicit MockAllocator() = default;
  explicit MockAllocator(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {
    EXPECT_TRUE(dispatcher_);
  }
  explicit MockAllocator(fidl::ServerEnd<fuchsia_sysmem2::Allocator> server_end) {
    binding_group_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                              std::move(server_end), this, fidl::kIgnoreBindingClosure);
  }

  zx_status_t Connect(fidl::ServerEnd<fuchsia_sysmem2::Allocator> request) {
    binding_group_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(request),
                              this, fidl::kIgnoreBindingClosure);
    return ZX_OK;
  }

  void BindSharedCollection(BindSharedCollectionRequestView request,
                            BindSharedCollectionCompleter::Sync& completer) override {
    display::DriverBufferCollectionId buffer_collection_id = next_buffer_collection_id_++;
    fbl::AutoLock lock(&lock_);
    active_buffer_collections_.emplace(
        buffer_collection_id,
        BufferCollection{.token_client = std::move(request->token()),
                         .unowned_collection_server = request->buffer_collection_request(),
                         .mock_buffer_collection = std::make_unique<StubBufferCollection>()});

    auto ref = fidl::BindServer(
        fdf::Dispatcher::GetCurrent()->async_dispatcher(),
        std::move(request->buffer_collection_request()),
        active_buffer_collections_.at(buffer_collection_id).mock_buffer_collection.get(),
        [this, buffer_collection_id](StubBufferCollection*, fidl::UnbindInfo,
                                     fidl::ServerEnd<fuchsia_sysmem2::BufferCollection>) {
          fbl::AutoLock lock(&lock_);
          inactive_buffer_collection_tokens_.push_back(
              std::move(active_buffer_collections_.at(buffer_collection_id).token_client));
          active_buffer_collections_.erase(buffer_collection_id);
        });
  }

  void SetDebugClientInfo(SetDebugClientInfoRequestView request,
                          SetDebugClientInfoCompleter::Sync& completer) override {
    EXPECT_EQ(request->name().get().find("virtio-gpu-display"), 0u);
  }

  std::vector<fidl::UnownedClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
  GetActiveBufferCollectionTokenClients() const {
    fbl::AutoLock lock(&lock_);
    std::vector<fidl::UnownedClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
        unowned_token_clients;
    unowned_token_clients.reserve(active_buffer_collections_.size());

    for (const auto& kv : active_buffer_collections_) {
      unowned_token_clients.push_back(kv.second.token_client);
    }
    return unowned_token_clients;
  }

  std::vector<fidl::UnownedClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
  GetInactiveBufferCollectionTokenClients() const {
    fbl::AutoLock lock(&lock_);
    std::vector<fidl::UnownedClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
        unowned_token_clients;
    unowned_token_clients.reserve(inactive_buffer_collection_tokens_.size());

    for (const auto& token : inactive_buffer_collection_tokens_) {
      unowned_token_clients.push_back(token);
    }
    return unowned_token_clients;
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    EXPECT_TRUE(false);
  }

 private:
  fidl::ServerBindingGroup<fuchsia_sysmem2::Allocator> binding_group_;

  struct BufferCollection {
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> token_client;
    fidl::UnownedServerEnd<fuchsia_sysmem2::BufferCollection> unowned_collection_server;
    std::unique_ptr<StubBufferCollection> mock_buffer_collection;
  };

  mutable fbl::Mutex lock_;
  std::unordered_map<display::DriverBufferCollectionId, BufferCollection> active_buffer_collections_
      __TA_GUARDED(lock_);
  std::vector<fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
      inactive_buffer_collection_tokens_ __TA_GUARDED(lock_);

  display::DriverBufferCollectionId next_buffer_collection_id_ =
      display::DriverBufferCollectionId(0);

  async_dispatcher_t* dispatcher_ = nullptr;
};

class FakeSysmem : public fidl::testing::WireTestBase<fuchsia_hardware_sysmem::Sysmem> {
 public:
  fuchsia_hardware_sysmem::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_sysmem::Service::InstanceHandler({
        .sysmem = sysmem_bindings_.CreateHandler(
            this, fdf::Dispatcher::GetCurrent()->async_dispatcher(), fidl::kIgnoreBindingClosure),
        .allocator_v1 = [](fidl::ServerEnd<fuchsia_sysmem::Allocator> request) {},
        .allocator_v2 =
            allocator_v2_.bind_handler(fdf::Dispatcher::GetCurrent()->async_dispatcher()),

    });
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  MockAllocator allocator_v2_;
  fidl::ServerBindingGroup<fuchsia_hardware_sysmem::Sysmem> sysmem_bindings_;
};

// Implement all the WireServer handlers of fuchsia_hardware_pci::Device as protocol as required by
// FIDL.
class FakePciParent : public fidl::WireServer<fuchsia_hardware_pci::Device> {
 public:
  fuchsia_hardware_pci::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_pci::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(
            this, fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher()),
            fidl::kIgnoreBindingClosure),
    });
  }
  void GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) override {
    fuchsia_hardware_pci::wire::DeviceInfo info;
    info.device_id = kTestDeviceId;
    completer.Reply(info);
  }
  void GetBar(GetBarRequestView request, GetBarCompleter::Sync& completer) override {
    fuchsia_hardware_pci::wire::Bar bar;
    completer.ReplySuccess(std::move(bar));
  }
  void SetBusMastering(SetBusMasteringRequestView request,
                       SetBusMasteringCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }
  void ResetDevice(ResetDeviceCompleter::Sync& completer) override { completer.ReplySuccess(); }
  void AckInterrupt(AckInterruptCompleter::Sync& completer) override { completer.ReplySuccess(); }
  void MapInterrupt(MapInterruptRequestView request,
                    MapInterruptCompleter::Sync& completer) override {
    zx::interrupt interrupt;
    completer.ReplySuccess(std::move(interrupt));
  }
  void GetInterruptModes(GetInterruptModesCompleter::Sync& completer) override {
    fuchsia_hardware_pci::wire::InterruptModes modes;
    completer.Reply(modes);
  }
  void SetInterruptMode(SetInterruptModeRequestView request,
                        SetInterruptModeCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }
  void ReadConfig8(ReadConfig8RequestView request, ReadConfig8Completer::Sync& completer) override {
    completer.ReplySuccess(0);
  }
  void ReadConfig16(ReadConfig16RequestView request,
                    ReadConfig16Completer::Sync& completer) override {
    completer.ReplySuccess(kTestSubsysDeviceId);
  }
  void ReadConfig32(ReadConfig32RequestView request,
                    ReadConfig32Completer::Sync& completer) override {
    completer.ReplySuccess(0);
  }
  void WriteConfig8(WriteConfig8RequestView request,
                    WriteConfig8Completer::Sync& completer) override {
    completer.ReplySuccess();
  }
  void WriteConfig16(WriteConfig16RequestView request,
                     WriteConfig16Completer::Sync& completer) override {
    completer.ReplySuccess();
  }
  void WriteConfig32(WriteConfig32RequestView request,
                     WriteConfig32Completer::Sync& completer) override {
    completer.ReplySuccess();
  }
  void GetCapabilities(GetCapabilitiesRequestView request,
                       GetCapabilitiesCompleter::Sync& completer) override {
    std::vector<uint8_t> empty_vec;
    auto empty_vec_view = fidl::VectorView<uint8_t>::FromExternal(empty_vec);
    completer.Reply(empty_vec_view);
  }
  void GetExtendedCapabilities(GetExtendedCapabilitiesRequestView request,
                               GetExtendedCapabilitiesCompleter::Sync& completer) override {
    std::vector<uint16_t> empty_vec;
    auto empty_vec_view = fidl::VectorView<uint16_t>::FromExternal(empty_vec);
    completer.Reply(empty_vec_view);
  }
  void GetBti(GetBtiRequestView request, GetBtiCompleter::Sync& completer) override {
    zx_handle_t fake_handle;
    fake_bti_create(&fake_handle);
    zx::bti bti(fake_handle);
    completer.ReplySuccess(std::move(bti));
  }
  fidl::ServerBindingGroup<fuchsia_hardware_pci::Device> binding_group_;
};

using fdf_testing::TestEnvironment;

class VirtioGpuTest : public ::testing::Test {
 public:
  VirtioGpuTest() = default;
  void SetUp() override {
    // Create start args.
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    EXPECT_OK(start_args);

    // Serve outgoing directory.
    driver_outgoing_ = std::move(start_args->outgoing_directory_client);
    auto svc_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    EXPECT_OK(svc_endpoints);
    zx_status_t status = fdio_open_at(driver_outgoing_.handle()->get(), "/svc",
                                      static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                      svc_endpoints->server.TakeChannel().release());
    EXPECT_OK(status);

    // Setup the test environment.
    zx::result init_result = test_environment_.SyncCall(
        &TestEnvironment::Initialize, std::move(start_args->incoming_directory_server));
    EXPECT_OK(init_result);

    auto pci_handler = fake_pci_parent_.SyncCall(&FakePciParent::GetInstanceHandler);
    test_environment_.SyncCall(
        [](TestEnvironment* env, auto handler) {
          zx::result result = env->incoming_directory().AddService<fuchsia_hardware_pci::Service>(
              std::move(handler));
          EXPECT_TRUE(result.is_ok());
        },
        std::move(pci_handler));

    // Serve Fake Sysmem.
    fuchsia_hardware_sysmem::Service::InstanceHandler sysmem_handler =
        sysmem_server_.SyncCall(&FakeSysmem::GetInstanceHandler);
    test_environment_.SyncCall(
        [](TestEnvironment* env, auto handler) {
          zx::result result =
              env->incoming_directory().AddService<fuchsia_hardware_sysmem::Service>(
                  std::move(handler));
          EXPECT_TRUE(result.is_ok());
        },
        std::move(sysmem_handler));

    // Start driver.
    zx::result start_result = runtime_.RunToCompletion(
        driver_.SyncCall(&fdf_testing::DriverUnderTest<virtio_display::GpuDeviceDriver>::Start,
                         std::move(start_args->start_args)));

    EXPECT_OK(start_result);
  }

  void TearDown() override {
    zx::result stop_result = runtime_.RunToCompletion(driver_.SyncCall(
        &fdf_testing::DriverUnderTest<virtio_display::GpuDeviceDriver>::PrepareStop));
    EXPECT_OK(stop_result);
  }

  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }

 private:
  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  // Env dispatcher runs in the background because we need to make sync calls into it.
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};
  async_patterns::TestDispatcherBound<TestEnvironment> test_environment_{env_dispatcher(),
                                                                         std::in_place};

  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<virtio_display::GpuDeviceDriver>>
      driver_{driver_dispatcher_->async_dispatcher(), std::in_place};
  async_patterns::TestDispatcherBound<FakePciParent> fake_pci_parent_{env_dispatcher(),
                                                                      std::in_place};
  async_patterns::TestDispatcherBound<FakeSysmem> sysmem_server_{env_dispatcher(), std::in_place};

  fidl::ClientEnd<fuchsia_io::Directory> driver_outgoing_;
};

TEST_F(VirtioGpuTest, CreateAndStartDriver) {}

}  // namespace
