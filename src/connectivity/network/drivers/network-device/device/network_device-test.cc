// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/fit/defer.h>
#include <lib/sync/cpp/completion.h>
#include <lib/syslog/global.h>

#include <future>
#include <iomanip>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "device_interface.h"
#include "log.h"
#include "network_device_shim.h"
#include "session.h"
#include "src/connectivity/network/drivers/network-device/mac/test_util.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/testing/predicates/status.h"
#include "test_session.h"
#include "test_util.h"
#include "test_util_banjo.h"

// Enable timeouts only to test things locally, committed code should not use timeouts.
#define ENABLE_TIMEOUTS 0

#if ENABLE_TIMEOUTS
#define TEST_DEADLINE zx::deadline_after(zx::msec(5000))
#else
#define TEST_DEADLINE zx::time::infinite()
#endif

namespace {

using ::testing::ElementsAreArray;

// Attempts to read an epitaph from |channel|. Returns the epitaph in the OK variant when it could
// be fetched.
zx::result<zx_status_t> WaitClosedAndReadEpitaph(const zx::channel& channel) {
  if (zx_status_t status = channel.wait_one(ZX_CHANNEL_PEER_CLOSED, TEST_DEADLINE, nullptr);
      status != ZX_OK) {
    return zx::error(status);
  }
  fidl_epitaph_t epitaph;
  uint32_t actual_bytes;
  if (zx_status_t status =
          channel.read(0, &epitaph, nullptr, sizeof(epitaph), 0, &actual_bytes, nullptr);
      status != ZX_OK) {
    return zx::error(status);
  }
  if (actual_bytes != sizeof(epitaph)) {
    return zx::error(ZX_ERR_BAD_STATE);
  }
  return zx::ok(epitaph.error);
}

std::string toHexString(cpp20::span<const uint8_t> data) {
  std::stringstream ss;
  for (const uint8_t& b : data) {
    ss << std::setw(2) << std::setfill('0') << std::hex << static_cast<int>(b);
  }
  return ss.str();
}

}  // namespace

namespace network {
namespace testing {

using netdev::wire::RxFlags;

class NetworkDeviceTest : public ::testing::Test {
 public:
  // A port identifier commonly used in tests.
  // A nonzero identifier is chosen to avoid default value traps.
  static constexpr uint8_t kPort13 = 13;

  // Common descriptor names, to avoid magic numbers.
  static constexpr uint16_t kDescriptorIndex0 = 0;
  static constexpr uint16_t kDescriptorIndex1 = 1;
  static constexpr uint16_t kDescriptorIndex2 = 2;
  static constexpr uint16_t kDescriptorIndex3 = 3;
  static constexpr uint16_t kDescriptorIndex4 = 4;

  // A minimally valid mock MacAddressing implementation.
  static constexpr mac_addr_protocol_ops_t kMockMacOps = {
      .get_address =
          [](void* ctx, mac_address_t* out_mac) {
            constexpr uint8_t kMac[] = {1, 2, 3, 4, 5, 6};
            std::copy(std::begin(kMac), std::end(kMac), out_mac->octets);
          },
      .get_features =
          [](void* ctx, features_t* out_features) {
            *out_features = {.supported_modes = SUPPORTED_MAC_FILTER_MODE_MULTICAST_FILTER};
          },
      .set_mode = [](void* ctx, mac_filter_mode_t mode, const mac_address_t* multicast_macs_list,
                     size_t multicast_macs_count) {},
  };

  void SetUp() override {
    auto impl_dispatcher = fdf::UnsynchronizedDispatcher::Create(
        {}, "", [this](fdf_dispatcher_t*) { impl_dispatcher_shutdown_.Signal(); });
    ASSERT_OK(impl_dispatcher.status_value());
    impl_dispatcher_ = std::move(impl_dispatcher.value());

    auto ifc_dispatcher = fdf::UnsynchronizedDispatcher::Create(
        {}, "", [this](fdf_dispatcher_t*) { ifc_dispatcher_shutdown_.Signal(); });
    ASSERT_OK(ifc_dispatcher.status_value());
    ifc_dispatcher_ = std::move(ifc_dispatcher.value());
    auto port_dispatcher = fdf::UnsynchronizedDispatcher::Create(
        {}, "", [this](fdf_dispatcher_t*) { port_dispatcher_shutdown_.Signal(); });
    ASSERT_OK(port_dispatcher.status_value());
    port_dispatcher_ = std::move(port_dispatcher.value());

    fx_logger_config_t log_cfg = {
        .min_severity = FX_LOG_TRACE,
        .tags = nullptr,
        .num_tags = 0,
    };
    fx_log_reconfigure(&log_cfg);
  }

  void TearDown() override { DiscardDeviceSync(); }

  void DiscardDeviceSync() {
    if (device_) {
      sync_completion_t completer;
      device_->Teardown([&completer]() {
        LOG_TRACE("Test: Teardown complete");
        sync_completion_signal(&completer);
      });
      ASSERT_OK(sync_completion_wait_deadline(&completer, TEST_DEADLINE.get()));
      impl_.WaitReleased();
      port13_.WaitPortRemoved();
      device_ = nullptr;
    }
    impl_dispatcher_.ShutdownAsync();
    impl_dispatcher_shutdown_.Wait();
    ifc_dispatcher_.ShutdownAsync();
    ifc_dispatcher_shutdown_.Wait();
    port_dispatcher_.ShutdownAsync();
    port_dispatcher_shutdown_.Wait();
  }

  static zx_status_t WaitEvents(const zx::event& events, zx_signals_t signals, zx::time deadline) {
    zx_status_t status = events.wait_one(signals, deadline, nullptr);
    if (status == ZX_OK) {
      events.signal(signals, 0);
    }
    return status;
  }

  [[nodiscard]] zx_status_t WaitStart(zx::time deadline = TEST_DEADLINE) {
    return WaitEvents(impl_.events(), kEventStartCompleted, deadline);
  }

  [[nodiscard]] zx_status_t WaitStartInitiated(zx::time deadline = TEST_DEADLINE) {
    return WaitEvents(impl_.events(), kEventStartInitiated, deadline);
  }

  [[nodiscard]] zx_status_t WaitStop(zx::time deadline = TEST_DEADLINE) {
    return WaitEvents(impl_.events(), kEventStop, deadline);
  }

  [[nodiscard]] zx_status_t WaitSessionStarted(zx::time deadline = TEST_DEADLINE) {
    return WaitEvents(impl_.events(), kEventSessionStarted, deadline);
  }

  [[nodiscard]] zx_status_t WaitSessionDied(zx::time deadline = TEST_DEADLINE) {
    return WaitEvents(impl_.events(), kEventSessionDied, deadline);
  }

  [[nodiscard]] zx_status_t WaitTx(zx::time deadline = TEST_DEADLINE) {
    return WaitEvents(impl_.events(), kEventTx, deadline);
  }

  [[nodiscard]] zx_status_t WaitRxAvailable(zx::time deadline = TEST_DEADLINE) {
    return WaitEvents(impl_.events(), kEventRxAvailable, deadline);
  }

  [[nodiscard]] zx_status_t WaitPortActiveChanged(const FakeNetworkPortImpl& port,
                                                  zx::time deadline = TEST_DEADLINE) {
    return WaitEvents(port.events(), kEventPortActiveChanged, deadline);
  }

  fdf::Dispatcher& dispatcher() { return impl_dispatcher_; }

  fidl::WireSyncClient<netdev::Device> OpenConnection() {
    auto [client_end, server_end] = fidl::Endpoints<netdev::Device>::Create();
    EXPECT_OK(device_->Bind(std::move(server_end)));
    return fidl::WireSyncClient(std::move(client_end));
  }

  netdev::wire::PortId GetSaltedPortId(uint8_t base_port_id) {
    auto* dev_iface = static_cast<internal::DeviceInterface*>(device_.get());
    // Sidestep thread safety to poke into device internals.
    //
    // Generally safe for test usage here as long as ports are always added to
    // the device on the main thread.
    uint8_t salt = [&dev_iface, &base_port_id]() __TA_NO_THREAD_SAFETY_ANALYSIS {
      return dev_iface->GetPortSalt(base_port_id);
    }();
    return {
        .base = base_port_id,
        .salt = salt,
    };
  }

  zx::result<fidl::WireSyncClient<netdev::Port>> OpenPort(uint8_t base_port_id) {
    return OpenPort(GetSaltedPortId(base_port_id));
  }

  zx::result<fidl::WireSyncClient<netdev::Port>> OpenPort(netdev::wire::PortId port_id) {
    auto [client_end, server_end] = fidl::Endpoints<netdev::Port>::Create();
    fidl::Status result = OpenConnection()->GetPort(port_id, std::move(server_end));
    if (result.status() != ZX_OK) {
      return zx::error(result.status());
    }
    return zx::ok(fidl::WireSyncClient(std::move(client_end)));
  }

  zx_status_t CreateDevice() {
    if (device_) {
      return ZX_ERR_INTERNAL;
    }
    zx::result device = impl_.CreateChild(
        DeviceInterfaceDispatchers(impl_dispatcher_, ifc_dispatcher_, port_dispatcher_));
    if (device.is_ok()) {
      device_ = std::move(device.value());
    }

    // When the device is about to complete startup install a one-time event handler for the RX
    // queue to trigger the start event. Netdevice is not fully started until it has triggered the
    // RX queue, AFTER NetworkDeviceImpl::Start has replied with its completer.
    impl_.SetOnStart([this] {
      SetEvtRxQueuePacketHandler([this](uint64_t key) {
        // It's possible to encounter a kSessionSwitchKey packet here depending on scheduling. That
        // packet is queued before Start is called but may not be processed until after Start has
        // come to this point. Ignore the kSessionSwitchKey and only signal completion on the
        // kTriggerRxKey packet as that is what actually signals that the RX queue is ready. And
        // that is the packet we need to consume to ensure it doesn't interfere with the tests.
        if (key == internal::RxQueue::kTriggerRxKey) {
          SetEvtRxQueuePacketHandler(nullptr);
          impl_.events().signal(0, kEventStartCompleted);
        }
      });
    });

    return device.status_value();
  }

  zx_status_t CreateDeviceWithPort13() {
    if (zx_status_t status = CreateDevice(); status != ZX_OK) {
      return status;
    }
    port13_.SetStatus({.mtu = 2048});
    return port13_.AddPort(kPort13, impl_dispatcher_.get(), OpenConnection(), impl_.client());
  }

  zx_status_t OpenSession(TestSession* session,
                          netdev::wire::SessionFlags flags = netdev::wire::SessionFlags::kPrimary,
                          uint16_t num_descriptors = kDefaultDescriptorCount,
                          uint64_t buffer_size = kDefaultBufferLength,
                          const char* session_name = nullptr) {
    // automatically increment to test_session_(a, b, c, etc...)
    char session_name_storage[] = "test_session_a";
    if (session_name == nullptr) {
      session_name_storage[strlen(session_name_storage) - 1] =
          static_cast<char>('a' + (session_counter_ % ('z' - 'a')));
      session_counter_++;
      session_name = session_name_storage;
    }

    fidl::WireSyncClient connection = OpenConnection();
    return session->Open(connection, session_name, flags, num_descriptors, buffer_size);
  }

  zx_status_t AttachSessionPort(TestSession& session, FakeNetworkPortImpl& impl) {
    std::vector<netdev::wire::FrameType> rx_types;
    for (netdev::wire::FrameType frame_type : impl.port_info().rx_types) {
      rx_types.push_back(frame_type);
    }
    return session.AttachPort(GetSaltedPortId(impl.id()), std::move(rx_types));
  }

  zx_status_t DetachSessionPort(TestSession& session, FakeNetworkPortImpl& impl) {
    return session.DetachPort(GetSaltedPortId(impl.id()));
  }

  const internal::SessionList& GetDeviceSessionsUnsafe(internal::DeviceInterface& device) const {
    return device.sessions_;
  }

  const internal::Session* GetPrimarySession(internal::DeviceInterface& device) const
      __TA_REQUIRES(device.control_lock_) {
    return device.primary_session_.get();
  }

  void SetEvtRxQueuePacketHandler(fit::function<void(uint64_t)> h) {
    static_cast<internal::DeviceInterface*>(device_.get())->evt_rx_queue_packet_.Set(std::move(h));
  }

  void SetEvtTxCompleteHandler(fit::function<void()> h) {
    static_cast<internal::DeviceInterface*>(device_.get())->evt_tx_complete_.Set(std::move(h));
  }

  void SetBacktraceCallback(fit::function<void()> cb) {
    auto* dev = static_cast<internal::DeviceInterface*>(device_.get());
    dev->diagnostics().trigger_stack_trace_ = std::move(cb);
  }

  // Create an RX queue packet event handler that will signal a completion once the RX queue has
  // been triggered. The completion is created and owned by the handler and a pointer to the
  // completion will be placed in the |out_completion| parameter. This ensures that even if the
  // event handler is called after the test has gone out of scope or as the event handler is being
  // reset it will not attempt to use a completion stored on the stack of the test.
  fit::function<void(uint64_t)> CreateTriggerRxHandler(libsync::Completion** out_completion) {
    std::unique_ptr completion = std::make_unique<libsync::Completion>();
    *out_completion = completion.get();
    return [completion = std::move(completion)](uint64_t key) {
      // Ignore any kFifoWatchKey events as they may occur before the kTriggerRxKey event when
      // completing RX buffers.
      if (key == internal::RxQueue::kFifoWatchKey) {
        return;
      }
      EXPECT_EQ(key, internal::RxQueue::kTriggerRxKey);
      completion->Signal();
    };
  }

 protected:
  fdf_testing::DriverRuntime driver_runtime_;
  fdf::UnsynchronizedDispatcher impl_dispatcher_;
  libsync::Completion impl_dispatcher_shutdown_;
  fdf::UnsynchronizedDispatcher ifc_dispatcher_;
  libsync::Completion ifc_dispatcher_shutdown_;
  fdf::UnsynchronizedDispatcher port_dispatcher_;
  libsync::Completion port_dispatcher_shutdown_;

  FakeNetworkDeviceImpl impl_;
  FakeNetworkPortImpl port13_;
  FakeMacDeviceImpl mac_impl_;
  int8_t session_counter_ = 0;
  std::unique_ptr<NetworkDeviceInterface> device_;
};

void PrintVec(const std::string& name, const std::vector<uint8_t>& vec) {
  printf("Vec %s: ", name.c_str());
  for (const auto& x : vec) {
    printf("%02X ", x);
  }
  printf("\n");
}

enum class RxTxSwitch {
  Rx,
  Tx,
};

const char* rxTxSwitchToString(RxTxSwitch rxtx) {
  switch (rxtx) {
    case RxTxSwitch::Tx:
      return "Tx";
    case RxTxSwitch::Rx:
      return "Rx";
  }
}

RxTxSwitch flipRxTxSwitch(RxTxSwitch rxtx) {
  switch (rxtx) {
    case RxTxSwitch::Tx:
      return RxTxSwitch::Rx;
    case RxTxSwitch::Rx:
      return RxTxSwitch::Tx;
  }
}

const std::string rxTxParamTestToString(const ::testing::TestParamInfo<RxTxSwitch>& info) {
  return rxTxSwitchToString(info.param);
}

// Helper class to instantiate test suites that have an Rx and Tx variant.
class RxTxParamTest : public NetworkDeviceTest, public ::testing::WithParamInterface<RxTxSwitch> {};

TEST_F(NetworkDeviceTest, CanCreate) { ASSERT_OK(CreateDevice()); }

TEST_F(NetworkDeviceTest, GetInfo) {
  impl_.info().min_rx_buffer_length = 2048;
  impl_.info().min_tx_buffer_length = 60;
  ASSERT_OK(CreateDevice());
  fidl::WireSyncClient connection = OpenConnection();
  fidl::WireResult rsp = connection->GetInfo();
  ASSERT_OK(rsp.status());
  auto& info = rsp.value().info;
  ASSERT_TRUE(info.has_descriptor_version());
  EXPECT_EQ(info.descriptor_version(), NETWORK_DEVICE_DESCRIPTOR_VERSION);
  ASSERT_TRUE(info.has_min_descriptor_length());
  EXPECT_EQ(info.min_descriptor_length(), sizeof(buffer_descriptor_t) / sizeof(uint64_t));
  ASSERT_TRUE(info.has_base_info());
  const auto& base_info = info.base_info();
  ASSERT_TRUE(base_info.has_tx_depth());
  EXPECT_EQ(base_info.tx_depth(), impl_.info().tx_depth * 2);
  ASSERT_TRUE(base_info.has_rx_depth());
  EXPECT_EQ(base_info.rx_depth(), impl_.info().rx_depth * 2);
  ASSERT_TRUE(base_info.has_min_rx_buffer_length());
  EXPECT_EQ(base_info.min_rx_buffer_length(), impl_.info().min_rx_buffer_length);
  ASSERT_TRUE(base_info.has_min_tx_buffer_length());
  EXPECT_EQ(base_info.min_tx_buffer_length(), impl_.info().min_tx_buffer_length);
  ASSERT_TRUE(base_info.has_max_buffer_length());
  EXPECT_EQ(base_info.max_buffer_length(), impl_.info().max_buffer_length);
  ASSERT_TRUE(base_info.has_max_buffer_parts());
  EXPECT_EQ(base_info.max_buffer_parts(), impl_.info().max_buffer_parts);
  ASSERT_TRUE(base_info.has_min_tx_buffer_tail());
  EXPECT_EQ(base_info.min_tx_buffer_tail(), impl_.info().tx_tail_length);
  ASSERT_TRUE(base_info.has_min_tx_buffer_head());
  EXPECT_EQ(base_info.min_tx_buffer_head(), impl_.info().tx_head_length);
  ASSERT_TRUE(base_info.has_buffer_alignment());
  EXPECT_EQ(base_info.buffer_alignment(), impl_.info().buffer_alignment);
  ASSERT_TRUE(base_info.has_tx_accel());
  EXPECT_EQ(base_info.tx_accel().count(), impl_.info().tx_accel.size());
  ASSERT_TRUE(base_info.has_rx_accel());
  EXPECT_EQ(base_info.rx_accel().count(), impl_.info().rx_accel.size());
}

TEST_F(NetworkDeviceTest, OptionalMaxBufferLength) {
  impl_.info().max_buffer_length = 0;
  ASSERT_OK(CreateDevice());
  fidl::WireSyncClient connection = OpenConnection();
  fidl::WireResult rsp = connection->GetInfo();
  ASSERT_OK(rsp.status());
  auto& info = rsp.value().info;
  ASSERT_TRUE(info.has_base_info());
  ASSERT_FALSE(info.base_info().has_max_buffer_length())
      << "Unexpected buffer length " << info.base_info().max_buffer_length();
}

TEST_F(NetworkDeviceTest, MinReportedBufferAlignment) {
  // Tests that device creation is rejected with an invalid buffer_alignment value.
  impl_.info().buffer_alignment = 0;
  ASSERT_STATUS(CreateDevice(), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(NetworkDeviceTest, InvalidRxThreshold) {
  // Tests that device creation is rejected with an invalid rx_threshold value.
  impl_.info().rx_threshold = impl_.info().rx_depth + 1;
  ASSERT_STATUS(CreateDevice(), ZX_ERR_NOT_SUPPORTED);
}

class PrepareVmoCallbackParamTest : public NetworkDeviceTest,
                                    public ::testing::WithParamInterface<bool> {
 public:
  void InstallPrepareVmoCallback(zx_status_t status) {
    impl_.set_prepare_vmo_handler([this, status](uint8_t, const zx::vmo&, auto& completer) {
      const bool deferred_callback = GetParam();
      if (deferred_callback) {
        async::PostTask(dispatcher().async_dispatcher(),
                        [completer = completer.ToAsync(), status]() mutable {
                          fdf::Arena arena('TEST');
                          completer.buffer(arena).Reply(status);
                        });
      } else {
        fdf::Arena arena('TEST');
        completer.buffer(arena).Reply(status);
      }
    });
  }
};

TEST_P(PrepareVmoCallbackParamTest, OpenSession) {
  InstallPrepareVmoCallback(ZX_OK);
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  for (uint16_t i = 0; i < 16; i++) {
    session.ResetDescriptor(i);
    session.SendRx(i);
  }
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  ASSERT_OK(WaitRxAvailable());
}

// Test that OpenSession fails if the device implementation rejects a VMO.
TEST_P(PrepareVmoCallbackParamTest, PrepareVmoFailure) {
  // Use any status here, the API contract is that the client should observe
  // ZX_ERR_INTERNAL.
  InstallPrepareVmoCallback(ZX_ERR_CANCELED);
  ASSERT_OK(CreateDevice());
  TestSession session;
  ASSERT_STATUS(OpenSession(&session), ZX_ERR_INTERNAL);
}

INSTANTIATE_TEST_SUITE_P(NetworkDeviceTest, PrepareVmoCallbackParamTest,
                         ::testing::Values(true, false),
                         [](const ::testing::TestParamInfo<bool>& info) {
                           if (info.param) {
                             return "Deferred";
                           }
                           return "Inline";
                         });

TEST_F(NetworkDeviceTest, RxBufferBuild) {
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());

  constexpr uint16_t kDescriptor0 = 0;
  constexpr uint16_t kDescriptor1 = 1;
  constexpr uint16_t kDescriptor2 = 2;

  constexpr struct {
    uint16_t space_head;
    uint16_t space_tail;
    uint16_t descriptor;
    uint32_t offset;
    uint32_t length;
    bool chain;
    std::optional<RxFlags> flags;
  } kDescriptorSetup[] = {{
                              .descriptor = kDescriptor0,
                              .length = 64,
                              .chain = false,
                              .flags = RxFlags::kRxAccel0,
                          },
                          {
                              .space_head = 16,
                              .descriptor = kDescriptor1,
                              .length = 15,
                              .chain = true,
                              .flags = RxFlags::kRxAccel1,
                          },
                          {
                              .space_tail = 32,
                              .descriptor = kDescriptor2,
                              .offset = 64,
                              .length = 8,
                              .chain = true,
                          }};
  for (const auto& setup : kDescriptorSetup) {
    buffer_descriptor_t& desc = session.ResetDescriptor(setup.descriptor);
    desc.head_length = setup.space_head;
    desc.tail_length = setup.space_tail;
    desc.data_length -= setup.space_head + setup.space_tail;
  }

  uint16_t all_descs[std::size(kDescriptorSetup)] = {kDescriptor0, kDescriptor1, kDescriptor2};
  size_t sent;
  ASSERT_OK(session.SendRx(all_descs, std::size(all_descs), &sent));
  ASSERT_EQ(sent, std::size(kDescriptorSetup));
  ASSERT_OK(WaitRxAvailable());

  // Get the expected VMO ID for all buffers.
  std::optional first_vmo = impl_.first_vmo_id();
  ASSERT_TRUE(first_vmo.has_value());
  uint8_t want_vmo = first_vmo.value();

  RxFidlReturnTransaction return_session(&impl_);

  // Prepare a chained return.
  auto chained_return = std::make_unique<RxFidlReturn>();
  fbl::DoublyLinkedList buffers = impl_.TakeRxBuffers();
  for (const auto& descriptor_setup : kDescriptorSetup) {
    SCOPED_TRACE(descriptor_setup.descriptor);
    // Load the buffers from the fake device implementation and check them.
    // We call "pop_back" on the buffer list because network_device feeds Rx buffers in a LIFO
    // order.
    std::unique_ptr rx = buffers.pop_back();
    ASSERT_TRUE(rx);
    const fuchsia_hardware_network_driver::wire::RxSpaceBuffer& space = rx->space();
    ASSERT_EQ(space.region.vmo, want_vmo);
    buffer_descriptor_t& descriptor = session.descriptor(descriptor_setup.descriptor);
    ASSERT_EQ(space.region.offset, descriptor.offset + descriptor.head_length);
    ASSERT_EQ(space.region.length, descriptor.data_length + descriptor.tail_length);

    rx->return_part().offset = descriptor_setup.offset;
    rx->return_part().length = descriptor_setup.length;
    if (descriptor_setup.chain) {
      if (descriptor_setup.flags.has_value()) {
        chained_return->buffer().meta.flags = static_cast<uint32_t>(*descriptor_setup.flags);
      }
      chained_return->PushPart(std::move(rx));
    } else {
      std::unique_ptr ret = std::make_unique<RxFidlReturn>(std::move(rx), kPort13);
      if (descriptor_setup.flags.has_value()) {
        ret->buffer().meta.flags = static_cast<uint32_t>(*descriptor_setup.flags);
      }
      return_session.Enqueue(std::move(ret));
    }
  }
  chained_return->buffer().meta.port = kPort13;
  chained_return->buffer().meta.flags = static_cast<uint32_t>(RxFlags::kRxAccel1);
  return_session.Enqueue(std::move(chained_return));
  // Ensure no more rx buffers were actually returned:
  ASSERT_TRUE(buffers.is_empty());

  libsync::Completion* completion = nullptr;
  SetEvtRxQueuePacketHandler(CreateTriggerRxHandler(&completion));
  //  Commit the returned buffers.
  return_session.Commit();
  ASSERT_OK(completion->Wait(TEST_DEADLINE));
  SetEvtRxQueuePacketHandler(nullptr);

  // Check that all descriptors were returned to the queue:
  size_t read_back;
  ASSERT_OK(session.FetchRx(all_descs, std::size(all_descs), &read_back));
  // We chained descriptors 2 descriptors together, so we should observe one less than the number of
  // descriptors returned.
  ASSERT_EQ(read_back, std::size(kDescriptorSetup) - 1);
  EXPECT_EQ(all_descs[0], kDescriptor0);
  EXPECT_EQ(all_descs[1], kDescriptor1);
  // Finally check all the stuff that was returned.
  for (const auto& setup : kDescriptorSetup) {
    SCOPED_TRACE(setup.descriptor);
    buffer_descriptor_t& desc = session.descriptor(setup.descriptor);
    EXPECT_EQ(desc.offset, session.canonical_offset(setup.descriptor));
    if (setup.descriptor == kDescriptor1) {
      // This descriptor should have a chain.
      EXPECT_EQ(desc.chain_length, 1u);
      EXPECT_EQ(desc.nxt, kDescriptor2);
    } else {
      EXPECT_EQ(desc.chain_length, 0u);
    }
    if (setup.descriptor == kDescriptor2) {
      // The chained descriptor's port metadata is not set.
      EXPECT_EQ(desc.port_id.base, 0);
      EXPECT_EQ(desc.port_id.salt, 0);
    } else {
      EXPECT_EQ(desc.port_id.base, kPort13);
      EXPECT_EQ(desc.port_id.salt, GetSaltedPortId(kPort13).salt);
    }
    if (setup.flags.has_value()) {
      EXPECT_EQ(desc.inbound_flags, static_cast<uint32_t>(*setup.flags));
    }
    EXPECT_EQ(desc.head_length, setup.offset);
    EXPECT_EQ(desc.data_length, setup.length);
    EXPECT_EQ(desc.tail_length, kDefaultBufferLength - setup.length - setup.offset);
  }
}

TEST_F(NetworkDeviceTest, TxBufferBuild) {
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  constexpr size_t kDescTests = 3;
  const netdev::wire::PortId port13_id = GetSaltedPortId(kPort13);
  // send three Rx descriptors:
  // - A simple descriptor with just data length
  // - A descriptor with head and tail removed
  // - A chained descriptor with simple data lengths.
  uint16_t all_descs[kDescTests + 1] = {0, 1, 2};
  {
    buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex0);
    desc.port_id = {
        .base = port13_id.base,
        .salt = port13_id.salt,
    };
  }
  {
    buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex1);
    desc.port_id = {
        .base = port13_id.base,
        .salt = port13_id.salt,
    };
    desc.head_length = 16;
    desc.tail_length = 32;
    desc.data_length -= desc.head_length + desc.tail_length;
  }
  {
    buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex2);
    desc.port_id = {
        .base = port13_id.base,
        .salt = port13_id.salt,
    };
    desc.data_length = 10;
    desc.chain_length = 2;
    desc.nxt = 3;
  }
  {
    buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex3);
    desc.data_length = 20;
    desc.chain_length = 1;
    desc.nxt = 4;
  }
  {
    buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex4);
    desc.data_length = 30;
    desc.chain_length = 0;
  }

  size_t sent;
  ASSERT_OK(session.SendTx(all_descs, kDescTests, &sent));
  ASSERT_EQ(sent, kDescTests);
  ASSERT_OK(WaitTx());
  TxFidlReturnTransaction return_session(&impl_);
  // load the buffers from the fake device implementation and check them.
  std::unique_ptr tx = impl_.PopTxBuffer();
  ASSERT_TRUE(tx);
  ASSERT_EQ(tx->buffer().data.count(), 1u);
  ASSERT_EQ(tx->buffer().data[0].offset, session.descriptor(kDescriptorIndex0).offset);
  ASSERT_EQ(tx->buffer().data[0].length, kDefaultBufferLength);
  return_session.Enqueue(std::move(tx));
  // check second descriptor:
  tx = impl_.PopTxBuffer();
  ASSERT_TRUE(tx);
  ASSERT_EQ(tx->buffer().data.count(), 1u);
  {
    buffer_descriptor_t& desc = session.descriptor(kDescriptorIndex1);
    ASSERT_EQ(tx->buffer().data[0].offset, desc.offset + desc.head_length);
    ASSERT_EQ(tx->buffer().data[0].length,
              kDefaultBufferLength - desc.head_length - desc.tail_length);
  }
  tx->set_status(ZX_ERR_UNAVAILABLE);
  return_session.Enqueue(std::move(tx));
  // check third descriptor:
  tx = impl_.PopTxBuffer();
  ASSERT_TRUE(tx);
  ASSERT_EQ(tx->buffer().data.count(), 3u);
  {
    uint16_t descriptor = 2;
    for (const fuchsia_hardware_network_driver::wire::BufferRegion& region :
         tx->buffer().data.get()) {
      SCOPED_TRACE(descriptor);
      buffer_descriptor_t& d = session.descriptor(descriptor++);
      ASSERT_EQ(region.offset, d.offset);
      ASSERT_EQ(region.length, d.data_length);
    }
  }
  tx->set_status(ZX_ERR_NOT_SUPPORTED);
  return_session.Enqueue(std::move(tx));
  // ensure no more tx buffers were actually enqueued:
  ASSERT_FALSE(impl_.PopTxBuffer());

  sync_completion_t completion;
  SetEvtTxCompleteHandler([&completion]() { sync_completion_signal(&completion); });
  // commit the returned buffers
  return_session.Commit();
  ASSERT_OK(sync_completion_wait_deadline(&completion, TEST_DEADLINE.get()));
  SetEvtTxCompleteHandler(nullptr);

  // check that all descriptors were returned to the queue:
  size_t read_back;

  ASSERT_OK(session.FetchTx(all_descs, kDescTests + 1, &read_back));
  ASSERT_EQ(read_back, kDescTests);
  EXPECT_EQ(all_descs[0], 0u);
  EXPECT_EQ(all_descs[1], 1u);
  EXPECT_EQ(all_descs[2], 2u);
  // check the status of the returned descriptors
  {
    buffer_descriptor_t& desc = session.descriptor(kDescriptorIndex0);
    EXPECT_EQ(desc.return_flags, 0u);
  }
  {
    buffer_descriptor_t& desc = session.descriptor(kDescriptorIndex1);
    EXPECT_EQ(desc.return_flags,
              static_cast<uint32_t>(netdev::wire::TxReturnFlags::kTxRetError |
                                    netdev::wire::TxReturnFlags::kTxRetNotAvailable));
  }
  {
    buffer_descriptor_t& desc = session.descriptor(kDescriptorIndex2);
    EXPECT_EQ(desc.return_flags,
              static_cast<uint32_t>(netdev::wire::TxReturnFlags::kTxRetError |
                                    netdev::wire::TxReturnFlags::kTxRetNotSupported));
  }
}

TEST_F(NetworkDeviceTest, SessionEpitaph) {
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  ASSERT_OK(session.Close());
  // Closing the session should cause a stop.
  ASSERT_OK(WaitStop());
  // Wait for epitaph to show up in channel.
  zx::result epitaph = WaitClosedAndReadEpitaph(session.session().client_end().channel());
  ASSERT_OK(epitaph.status_value());
  ASSERT_STATUS(epitaph.value(), ZX_ERR_CANCELED);
}

TEST_F(NetworkDeviceTest, SessionPauseUnpause) {
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  // pausing and unpausing the session makes the device start and stop:
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  ASSERT_OK(DetachSessionPort(session, port13_));
  ASSERT_OK(WaitStop());
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  ASSERT_OK(DetachSessionPort(session, port13_));
  ASSERT_OK(WaitStop());
}

TEST_F(NetworkDeviceTest, TwoSessionsTx) {
  ASSERT_OK(CreateDeviceWithPort13());
  const netdev::wire::PortId port13_id = GetSaltedPortId(kPort13);
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session_a;
  ASSERT_OK(OpenSession(&session_a));
  TestSession session_b;
  ASSERT_OK(OpenSession(&session_b));
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(AttachSessionPort(session_b, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());
  // Send something from each session, both should succeed.
  std::vector<uint8_t> sent_buff_a({1, 2, 3, 4});
  std::vector<uint8_t> sent_buff_b({5, 6});
  session_a.SendTxData(port13_id, 0, sent_buff_a);
  ASSERT_OK(WaitTx());
  session_b.SendTxData(port13_id, 1, sent_buff_b);
  ASSERT_OK(WaitTx());
  // Wait until we have two frames waiting.
  std::unique_ptr buff_a = impl_.PopTxBuffer();
  std::unique_ptr buff_b = impl_.PopTxBuffer();
  VmoProvider vmo_provider = impl_.VmoGetter();
  zx::result data_status_a = buff_a->GetData(vmo_provider);
  ASSERT_OK(data_status_a.status_value());
  std::vector data_a = std::move(data_status_a.value());

  zx::result data_status_b = buff_b->GetData(vmo_provider);
  ASSERT_OK(data_status_b.status_value());
  std::vector data_b = std::move(data_status_b.value());
  // Can't rely on ordering here.
  if (data_a.size() != sent_buff_a.size()) {
    std::swap(buff_a, buff_b);
    std::swap(data_a, data_b);
  }
  PrintVec("data_a", data_a);
  PrintVec("data_b", data_b);
  ASSERT_EQ(data_a, sent_buff_a);
  ASSERT_EQ(data_b, sent_buff_b);
  // Return both buffers and ensure they get to the correct sessions.
  buff_a->set_status(ZX_OK);
  buff_b->set_status(ZX_ERR_UNAVAILABLE);
  TxFidlReturnTransaction tx_ret(&impl_);
  tx_ret.Enqueue(std::move(buff_a));
  tx_ret.Enqueue(std::move(buff_b));

  libsync::Completion completion;
  SetEvtTxCompleteHandler([&completion]() { completion.Signal(); });
  tx_ret.Commit();
  ASSERT_OK(completion.Wait(TEST_DEADLINE));
  SetEvtTxCompleteHandler(nullptr);

  uint16_t rd;
  ASSERT_OK(session_a.FetchTx(&rd));
  ASSERT_EQ(rd, 0u);
  ASSERT_OK(session_b.FetchTx(&rd));
  ASSERT_EQ(rd, 1u);
  ASSERT_EQ(session_a.descriptor(kDescriptorIndex0).return_flags, 0u);
  ASSERT_EQ(session_b.descriptor(kDescriptorIndex1).return_flags,
            static_cast<uint32_t>(netdev::wire::TxReturnFlags::kTxRetError |
                                  netdev::wire::TxReturnFlags::kTxRetNotAvailable));
}

TEST_F(NetworkDeviceTest, TwoSessionsRx) {
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session_a;
  ASSERT_OK(OpenSession(&session_a));
  TestSession session_b;
  ASSERT_OK(OpenSession(&session_b));
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(AttachSessionPort(session_b, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());
  constexpr uint16_t kBufferCount = 5;
  constexpr size_t kDataLen = 15;
  uint16_t desc_buff[kBufferCount];
  for (uint16_t i = 0; i < kBufferCount; i++) {
    session_a.ResetDescriptor(i);
    session_b.ResetDescriptor(i);
    desc_buff[i] = i;
  }
  ASSERT_OK(session_a.SendRx(desc_buff, kBufferCount, nullptr));
  ASSERT_OK(session_b.SendRx(desc_buff, kBufferCount, nullptr));

  ASSERT_OK(WaitRxAvailable());
  VmoProvider vmo_provider = impl_.VmoGetter();
  RxFidlReturnTransaction return_session(&impl_);
  for (uint16_t i = 0; i < kBufferCount; i++) {
    std::unique_ptr buff = impl_.PopRxBuffer();
    ASSERT_TRUE(buff);
    std::vector<uint8_t> data(kDataLen, static_cast<uint8_t>(i));
    ASSERT_OK(buff->WriteData(data, vmo_provider));
    return_session.Enqueue(std::move(buff), kPort13);
  }
  libsync::Completion* completion = nullptr;
  SetEvtRxQueuePacketHandler(CreateTriggerRxHandler(&completion));
  return_session.Commit();
  ASSERT_OK(completion->Wait(TEST_DEADLINE));
  SetEvtRxQueuePacketHandler(nullptr);

  auto checker = [kBufferCount, kDataLen](TestSession& session) {
    uint16_t descriptors[kBufferCount];
    size_t rd;
    ASSERT_OK(session.FetchRx(descriptors, kBufferCount, &rd));
    ASSERT_EQ(rd, kBufferCount);
    for (uint32_t i = 0; i < kBufferCount; i++) {
      buffer_descriptor_t& desc = session.descriptor(descriptors[i]);
      ASSERT_EQ(desc.data_length, kDataLen);
      auto* data = session.buffer(desc.offset);
      for (uint32_t j = 0; j < kDataLen; j++) {
        ASSERT_EQ(*data, static_cast<uint8_t>(i));
        data++;
      }
    }
  };
  {
    SCOPED_TRACE("session_a");
    checker(session_a);
  }
  {
    SCOPED_TRACE("session_b");
    checker(session_b);
  }
}

TEST_F(NetworkDeviceTest, ListenSession) {
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session_a;
  ASSERT_OK(OpenSession(&session_a));
  TestSession session_b;
  ASSERT_OK(OpenSession(&session_b, netdev::wire::SessionFlags::kListenTx));
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(AttachSessionPort(session_b, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());
  // Get an Rx descriptor ready on session b:
  session_b.ResetDescriptor(kDescriptorIndex0);
  ASSERT_OK(session_b.SendRx(kDescriptorIndex0));

  // send data from session a:
  std::vector<uint8_t> send_buff({1, 2, 3, 4});
  session_a.SendTxData(GetSaltedPortId(kPort13), 0, send_buff);
  ASSERT_OK(WaitTx());

  uint16_t desc_idx;
  ASSERT_OK(session_b.FetchRx(&desc_idx));
  ASSERT_EQ(desc_idx, 0u);
  buffer_descriptor_t& desc = session_b.descriptor(kDescriptorIndex0);
  ASSERT_EQ(desc.data_length, send_buff.size());
  auto* data = session_b.buffer(desc.offset);
  EXPECT_THAT(cpp20::span(data, send_buff.size()), ElementsAreArray(send_buff));
}

TEST_F(NetworkDeviceTest, ClosingPrimarySession) {
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session_a;
  ASSERT_OK(OpenSession(&session_a));
  TestSession session_b;
  ASSERT_OK(OpenSession(&session_b));
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(AttachSessionPort(session_b, port13_));
  ASSERT_OK(WaitSessionStarted());
  buffer_descriptor_t& d = session_a.ResetDescriptor(kDescriptorIndex0);
  d.data_length = kDefaultBufferLength / 2;
  session_b.ResetDescriptor(kDescriptorIndex1);
  ASSERT_OK(session_a.SendRx(kDescriptorIndex0));
  ASSERT_OK(WaitRxAvailable());
  // Implementation now owns session a's RxBuffer.
  std::unique_ptr rx_buff = impl_.PopRxBuffer();
  ASSERT_EQ(rx_buff->space().region.length, kDefaultBufferLength / 2);
  // Let's close session_a, it should not be closed until we return the buffers.
  ASSERT_OK(session_a.Close());
  ASSERT_EQ(session_a.session().client_end().channel().wait_one(
                ZX_CHANNEL_PEER_CLOSED, zx::deadline_after(zx::msec(20)), nullptr),
            ZX_ERR_TIMED_OUT);
  // Session B should've now become primary. Provide enough buffers to fill the device queues.
  uint16_t target_descriptor = 0;
  while (impl_.rx_buffer_count() < impl_.info().rx_depth - 1) {
    session_b.ResetDescriptor(target_descriptor);
    ASSERT_OK(session_b.SendRx(target_descriptor++));
    ASSERT_OK(WaitRxAvailable());
  }
  // Send one more descriptor that will receive the copied data form the old buffer in Session A.
  session_b.ResetDescriptor(target_descriptor);
  ASSERT_OK(session_b.SendRx(target_descriptor));

  // And now return data.
  constexpr uint32_t kReturnLength = 5;
  rx_buff->SetReturnLength(kReturnLength);
  RxFidlReturnTransaction rx_transaction(&impl_);
  rx_transaction.Enqueue(std::move(rx_buff), kPort13);
  rx_transaction.Commit();

  // Session a should be closed...
  ASSERT_OK(session_a.WaitClosed(TEST_DEADLINE));
  /// ...and Session b should still receive the data.
  uint16_t desc;
  ASSERT_OK(session_b.FetchRx(&desc));
  ASSERT_EQ(desc, target_descriptor);
  ASSERT_EQ(session_b.descriptor(desc).data_length, kReturnLength);
}

TEST_F(NetworkDeviceTest, DelayedStart) {
  ASSERT_OK(CreateDeviceWithPort13());
  impl_.set_auto_start(std::nullopt);
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session_a;
  ASSERT_OK(OpenSession(&session_a));
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitSessionStarted());
  // we're dealing starting the device, so the start must've been initiated.
  ASSERT_OK(WaitStartInitiated());
  // But we haven't actually called the callback.
  // We should be able to pause and unpause session_a while we're still holding the device.
  // we can send Tx data and it won't reach the device until TriggerStart is called.
  buffer_descriptor_t& desc = session_a.ResetDescriptor(kDescriptorIndex0);
  desc.port_id = {
      .base = kPort13,
      .salt = GetSaltedPortId(kPort13).salt,
  };
  ASSERT_OK(session_a.SendTx(kDescriptorIndex0));
  ASSERT_OK(DetachSessionPort(session_a, port13_));
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_FALSE(impl_.PopRxBuffer());
  ASSERT_TRUE(impl_.TriggerStart());
  ASSERT_OK(WaitTx());
  std::unique_ptr tx_buffer = impl_.PopTxBuffer();
  ASSERT_TRUE(tx_buffer);
  TxFidlReturnTransaction transaction(&impl_);
  transaction.Enqueue(std::move(tx_buffer));
  transaction.Commit();

  // pause the session again and wait for stop.
  ASSERT_OK(DetachSessionPort(session_a, port13_));
  ASSERT_OK(WaitStop());
  // Then unpause and re-pause the session:
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());
  // Pause the session once again, we haven't called TriggerStart yet.
  ASSERT_OK(DetachSessionPort(session_a, port13_));

  // As soon as we call TriggerStart, stop must be called, but not before
  ASSERT_STATUS(WaitStop(zx::deadline_after(zx::msec(20))), ZX_ERR_TIMED_OUT);
  ASSERT_TRUE(impl_.TriggerStart());
  ASSERT_OK(WaitStop());
}

TEST_F(NetworkDeviceTest, DelayedStop) {
  ASSERT_OK(CreateDeviceWithPort13());
  impl_.set_auto_stop(false);
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session_a;
  ASSERT_OK(OpenSession(&session_a));
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());

  ASSERT_OK(DetachSessionPort(session_a, port13_));
  ASSERT_OK(WaitStop());
  // Unpause the session again, we haven't called TriggerStop yet
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitSessionStarted());
  // As soon as we call TriggerStop, start must be called, but not before
  ASSERT_STATUS(WaitStart(zx::deadline_after(zx::msec(20))), ZX_ERR_TIMED_OUT);
  ASSERT_TRUE(impl_.TriggerStop());
  ASSERT_OK(WaitStart());

  // With the session running, send down a tx frame and then close the session. The session should
  // NOT be closed until we actually both call TriggerStop and return the outstanding buffer.
  buffer_descriptor_t& desc = session_a.ResetDescriptor(kDescriptorIndex0);
  desc.port_id = {
      .base = kPort13,
      .salt = GetSaltedPortId(kPort13).salt,
  };
  ASSERT_OK(session_a.SendTx(kDescriptorIndex0));
  ASSERT_OK(WaitTx());
  ASSERT_OK(session_a.Close());
  ASSERT_OK(WaitStop());
  // Session must not have been closed yet.
  ASSERT_EQ(session_a.session().client_end().channel().wait_one(
                ZX_CHANNEL_PEER_CLOSED, zx::deadline_after(zx::msec(20)), nullptr),
            ZX_ERR_TIMED_OUT);
  ASSERT_TRUE(impl_.TriggerStop());

  // Session must not have been closed yet.
  ASSERT_EQ(session_a.session().client_end().channel().wait_one(
                ZX_CHANNEL_PEER_CLOSED, zx::deadline_after(zx::msec(20)), nullptr),
            ZX_ERR_TIMED_OUT);

  // Return the outstanding buffer.
  std::unique_ptr buffer = impl_.PopTxBuffer();
  TxFidlReturnTransaction transaction(&impl_);
  transaction.Enqueue(std::move(buffer));
  transaction.Commit();
  // Now session should close.
  ASSERT_OK(session_a.WaitClosed(TEST_DEADLINE));
}

TEST_P(RxTxParamTest, WaitsForAllBuffersReturned) {
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  session.ResetDescriptor(kDescriptorIndex0);
  ASSERT_OK(session.SendRx(kDescriptorIndex0));
  buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex1);
  desc.port_id = {
      .base = kPort13,
      .salt = GetSaltedPortId(kPort13).salt,
  };
  ASSERT_OK(session.SendTx(kDescriptorIndex1));
  ASSERT_OK(WaitTx());
  ASSERT_OK(WaitRxAvailable());

  fbl::DoublyLinkedList rx_buffers = impl_.TakeRxBuffers();
  ASSERT_EQ(rx_buffers.size(), 1u);
  fbl::DoublyLinkedList tx_buffers = impl_.TakeTxBuffers();
  ASSERT_EQ(tx_buffers.size(), 1u);

  ASSERT_OK(session.Close());
  ASSERT_OK(WaitStop());

  // Session will not close until we return the buffers we're holding.
  ASSERT_STATUS(session.WaitClosed(zx::deadline_after(zx::msec(10))), ZX_ERR_TIMED_OUT);

  // Test parameter controls which buffers we'll return first.
  auto return_buffer = [this, &tx_buffers, &rx_buffers](RxTxSwitch which) {
    switch (which) {
      case RxTxSwitch::Tx: {
        TxFidlReturnTransaction transaction(&impl_);
        std::unique_ptr buffer = tx_buffers.pop_front();
        buffer->set_status(ZX_ERR_UNAVAILABLE);
        transaction.Enqueue(std::move(buffer));
        transaction.Commit();
      } break;
      case RxTxSwitch::Rx: {
        RxFidlReturnTransaction transaction(&impl_);
        std::unique_ptr buffer = rx_buffers.pop_front();
        buffer->return_part().length = 0;
        transaction.Enqueue(std::move(buffer), kPort13);
        transaction.Commit();
      } break;
    }
  };

  return_buffer(GetParam());
  ASSERT_STATUS(session.WaitClosed(zx::deadline_after(zx::msec(10))), ZX_ERR_TIMED_OUT);
  return_buffer(flipRxTxSwitch(GetParam()));
  ASSERT_OK(session.WaitClosed(TEST_DEADLINE));
}

TEST_F(NetworkDeviceTest, Teardown) {
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session_a;
  ASSERT_OK(OpenSession(&session_a));
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitSessionStarted());
  TestSession session_b;
  ASSERT_OK(OpenSession(&session_b));
  ASSERT_OK(AttachSessionPort(session_b, port13_));
  ASSERT_OK(WaitSessionStarted());
  TestSession session_c;
  ASSERT_OK(OpenSession(&session_c));

  DiscardDeviceSync();
  session_a.WaitClosed(TEST_DEADLINE);
  session_b.WaitClosed(TEST_DEADLINE);
  session_c.WaitClosed(TEST_DEADLINE);
}

TEST_F(NetworkDeviceTest, TeardownWithReclaim) {
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session_a;
  ASSERT_OK(OpenSession(&session_a));
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitStart());
  session_a.ResetDescriptor(kDescriptorIndex0);
  ASSERT_OK(session_a.SendRx(kDescriptorIndex0));
  buffer_descriptor_t& desc = session_a.ResetDescriptor(kDescriptorIndex1);
  desc.port_id = {
      .base = kPort13,
      .salt = GetSaltedPortId(kPort13).salt,
  };
  ASSERT_OK(session_a.SendTx(kDescriptorIndex1));
  ASSERT_OK(WaitTx());
  ASSERT_OK(WaitRxAvailable());
  ASSERT_EQ(impl_.rx_buffer_count(), 1u);
  ASSERT_EQ(impl_.tx_buffer_count(), 1u);

  DiscardDeviceSync();
  session_a.WaitClosed(TEST_DEADLINE);
}

TEST_F(NetworkDeviceTest, TxHeadLength) {
  constexpr uint16_t kHeadLength = 16;
  impl_.info().tx_head_length = kHeadLength;
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  session.ZeroVmo();
  const netdev::wire::PortId port13_id = GetSaltedPortId(kPort13);
  {
    buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex0);
    desc.port_id = {
        .base = port13_id.base,
        .salt = port13_id.salt,
    };
    desc.head_length = kHeadLength;
    desc.data_length = 1;
    *session.buffer(desc.offset + desc.head_length) = 0xAA;
  }
  {
    buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex1);
    desc.port_id = {
        .base = port13_id.base,
        .salt = port13_id.salt,
    };
    desc.head_length = kHeadLength * 2;
    desc.data_length = 1;
    *session.buffer(desc.offset + desc.head_length) = 0xBB;
  }
  uint16_t descs[] = {0, 1};
  size_t sent;
  ASSERT_OK(session.SendTx(descs, 2, &sent));
  ASSERT_EQ(sent, 2u);
  ASSERT_OK(WaitTx());

  VmoProvider vmo_provider = impl_.VmoGetter();
  TxFidlReturnTransaction transaction(&impl_);
  constexpr struct {
    uint8_t expect;
    const char* name;
  } kCheckTable[] = {
      {
          .expect = 0xAA,
          .name = "first buffer",
      },
      {
          .expect = 0xBB,
          .name = "second buffer",
      },
  };
  for (const auto& check : kCheckTable) {
    SCOPED_TRACE(check.name);
    std::unique_ptr buffer = impl_.PopTxBuffer();
    ASSERT_TRUE(buffer);
    ASSERT_EQ(buffer->buffer().head_length, kHeadLength);
    zx::result status = buffer->GetData(vmo_provider);
    ASSERT_OK(status.status_value());
    std::vector<uint8_t>& data = status.value();
    ASSERT_EQ(data.size(), kHeadLength + 1u);
    ASSERT_EQ(data[kHeadLength], check.expect);
    transaction.Enqueue(std::move(buffer));
  }
  transaction.Commit();
}

TEST_F(NetworkDeviceTest, InvalidTxFrameType) {
  constexpr netdev::wire::FrameType kDescriptorFrameType = netdev::wire::FrameType::kIpv4;
  port13_.SetSupportedRxType(kDescriptorFrameType);
  port13_.SetSupportedTxType(netdev::wire::FrameType::kEthernet);
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex0);
  desc.port_id = {
      .base = kPort13,
      .salt = GetSaltedPortId(kPort13).salt,
  };
  desc.frame_type = static_cast<uint8_t>(kDescriptorFrameType);
  ASSERT_OK(session.SendTx(kDescriptorIndex0));
  // Session should be killed because of contract breach:
  ASSERT_OK(session.WaitClosed(TEST_DEADLINE));
  // We should NOT have received that frame:
  ASSERT_FALSE(impl_.PopTxBuffer());
}

TEST_F(NetworkDeviceTest, RxFrameTypeFilter) {
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  session.ResetDescriptor(kDescriptorIndex0);
  ASSERT_OK(session.SendRx(kDescriptorIndex0));
  ASSERT_OK(WaitRxAvailable());
  std::unique_ptr buff = impl_.PopRxBuffer();
  buff->SetReturnLength(10);
  std::unique_ptr ret = std::make_unique<RxFidlReturn>(std::move(buff), kPort13);
  ret->buffer().meta.frame_type = netdev::wire::FrameType::kIpv4;
  RxFidlReturnTransaction rx_transaction(&impl_);
  rx_transaction.Enqueue(std::move(ret));
  rx_transaction.Commit();

  uint16_t ret_desc;
  ASSERT_EQ(session.FetchRx(&ret_desc), ZX_ERR_SHOULD_WAIT);
}

TEST_F(NetworkDeviceTest, ObserveStatus) {
  using netdev::wire::StatusFlags;
  ASSERT_OK(CreateDeviceWithPort13());
  auto [client_end, server_end] = fidl::Endpoints<netdev::StatusWatcher>::Create();
  fidl::WireSyncClient watcher{std::move(client_end)};

  zx::result port = OpenPort(kPort13);
  ASSERT_OK(port.status_value());
  ASSERT_OK(port->GetStatusWatcher(std::move(server_end), 3).status());
  {
    fidl::WireResult result = watcher->WatchStatus();
    ASSERT_OK(result.status());
    ASSERT_EQ(result.value().port_status.mtu(), port13_.status().mtu);
    ASSERT_EQ(result.value().port_status.flags(), StatusFlags());
  }
  // Set online, then set offline (watcher is buffered, we should be able to observe both).
  port13_.SetOnline(true);
  port13_.SetOnline(false);
  {
    fidl::WireResult result = watcher->WatchStatus();
    ASSERT_OK(result.status());
    ASSERT_EQ(result.value().port_status.mtu(), port13_.status().mtu);
    ASSERT_EQ(result.value().port_status.flags(), StatusFlags::kOnline);
  }
  {
    fidl::WireResult result = watcher->WatchStatus();
    ASSERT_OK(result.status());
    ASSERT_EQ(result.value().port_status.mtu(), port13_.status().mtu);
    ASSERT_EQ(result.value().port_status.flags(), StatusFlags());
  }

  DiscardDeviceSync();

  // Watcher must be closed on teardown.
  ASSERT_OK(
      watcher.client_end().channel().wait_one(ZX_CHANNEL_PEER_CLOSED, TEST_DEADLINE, nullptr));
}

// Test that returning tx buffers in the body of QueueTx is allowed and works.
TEST_F(NetworkDeviceTest, ReturnTxInline) {
  impl_.set_immediate_return_tx(true);
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  {
    buffer_descriptor_t& desc = session.ResetDescriptor(0x02);
    desc.port_id = {
        .base = kPort13,
        .salt = GetSaltedPortId(kPort13).salt,
    };
  }
  ASSERT_OK(session.SendTx(0x02));
  ASSERT_OK(session.tx_fifo().wait_one(ZX_FIFO_READABLE, TEST_DEADLINE, nullptr));
  uint16_t desc;
  ASSERT_OK(session.FetchTx(&desc));
  EXPECT_EQ(desc, 0x02);
}

// Test that attaching a session with unknown Rx types will fail.
TEST_F(NetworkDeviceTest, RejectsInvalidRxTypes) {
  constexpr netdev::wire::FrameType kDescriptorFrameType = netdev::wire::FrameType::kIpv4;
  port13_.SetSupportedRxType(netdev::wire::FrameType::kEthernet);
  port13_.SetSupportedTxType(kDescriptorFrameType);
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session, netdev::wire::SessionFlags::kPrimary, kDefaultDescriptorCount,
                        kDefaultBufferLength));
  ASSERT_STATUS(session.AttachPort(GetSaltedPortId(kPort13), {kDescriptorFrameType}),
                ZX_ERR_INVALID_ARGS);
}

// Regression test for session name not respecting fidl::StringView lack of null termination
// character.
TEST_F(NetworkDeviceTest, SessionNameRespectsStringView) {
  ASSERT_OK(CreateDeviceWithPort13());
  // Cast to internal implementation to access methods directly.
  auto* dev = static_cast<internal::DeviceInterface*>(device_.get());

  TestSession test_session;
  ASSERT_OK(test_session.Init(kDefaultDescriptorCount, kDefaultBufferLength));
  zx::result info_status = test_session.GetInfo();
  ASSERT_OK(info_status.status_value());
  netdev::wire::SessionInfo& info = info_status.value();

  const char* name_str = "hello world";
  // String view only contains "hello".
  fidl::StringView name = fidl::StringView::FromExternal(name_str, 5u);

  bool reply_called = false;
  libsync::Completion reply_completer;
  std::vector<zx::handle> handles;
  class T : public fidl::Transaction {
   public:
    explicit T(bool* r, libsync::Completion* reply_completer, std::vector<zx::handle>* handles)
        : reply_called_(r), reply_completer_(reply_completer), handles_(handles) {}
    std::unique_ptr<Transaction> TakeOwnership() override {
      auto t = std::make_unique<T>(reply_called_, reply_completer_, handles_);
      reply_called_ = nullptr;
      reply_completer_ = nullptr;
      handles_ = nullptr;
      return t;
    }
    zx_status_t Reply(fidl::OutgoingMessage* m, fidl::WriteOptions) override {
      fidl::OutgoingMessage& message = *m;
      // We have to store the handles as if the message was sent, since the
      // channel that encodes the lifetime of the created session is somewhere
      // in the outgoing message's handles.
      for (const fidl_handle_t& handle : cpp20::span(message.handles(), message.handle_actual())) {
        handles_->emplace_back(handle);
      }
      message.ReleaseHandles();
      *reply_called_ = true;
      reply_completer_->Signal();
      return ZX_OK;
    }
    void Close(zx_status_t epitaph) override {
      ADD_FAILURE() << "Unexpected call to Close with " << zx_status_get_string(epitaph);
    }

   private:
    bool* reply_called_;
    libsync::Completion* reply_completer_;
    std::vector<zx::handle>* handles_;
  } transaction(&reply_called, &reply_completer, &handles);

  fidl::WireServer<netdev::Device>::OpenSessionCompleter::Sync completer(&transaction);
  fidl::WireRequest<netdev::Device::OpenSession> req{name, info};
  fidl::WireServer<netdev::Device>::OpenSessionRequestView view(&req);
  dev->OpenSession(view, completer);
  ASSERT_OK(reply_completer.Wait(TEST_DEADLINE));
  ASSERT_TRUE(reply_called);
  const internal::SessionList& sessions = GetDeviceSessionsUnsafe(*dev);
  ASSERT_FALSE(sessions.is_empty());
  ASSERT_STREQ("hello", sessions.front().name());
}

TEST_F(NetworkDeviceTest, RejectsSmallRxBuffers) {
  constexpr uint32_t kMinRxLength = 60;
  impl_.info().min_rx_buffer_length = kMinRxLength;
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex0);
  desc.data_length = kMinRxLength - 1;
  ASSERT_OK(session.SendRx(kDescriptorIndex0));
  // Session should be killed because of contract breach:
  ASSERT_OK(session.WaitClosed(TEST_DEADLINE));
  // We should NOT have received that frame:
  ASSERT_FALSE(impl_.PopRxBuffer());
}

TEST_F(NetworkDeviceTest, RejectsSmallTxBuffers) {
  constexpr uint32_t kMinTxLength = 60;
  impl_.info().min_tx_buffer_length = kMinTxLength;
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex0);
  desc.port_id = {
      .base = kPort13,
      .salt = GetSaltedPortId(kPort13).salt,
  };
  desc.data_length = kMinTxLength - 1;
  ASSERT_OK(session.SendTx(kDescriptorIndex0));
  // Session should be killed because of contract breach:
  ASSERT_OK(session.WaitClosed(TEST_DEADLINE));
  // We should NOT have received that frame:
  ASSERT_FALSE(impl_.PopTxBuffer());
}

TEST_F(NetworkDeviceTest, RespectsRxThreshold) {
  constexpr uint64_t kReturnBufferSize = 1;
  ASSERT_OK(CreateDeviceWithPort13());
  fidl::WireSyncClient connection = OpenConnection();
  TestSession session;
  uint16_t descriptor_count = impl_.info().rx_depth * 2;
  ASSERT_OK(OpenSession(&session, netdev::wire::SessionFlags::kPrimary, descriptor_count));

  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());

  std::vector<uint16_t> descriptors;
  descriptors.reserve(descriptor_count);
  for (uint16_t i = 0; i < descriptor_count; i++) {
    session.ResetDescriptor(i);
    descriptors.push_back(i);
  }

  // Fill up to half depth one buffer at a time, waiting for each one to be observed by the device
  // driver implementation. The slow dripping of buffers will force the Rx queue to enter
  // steady-state so we're not racing the return buffer signals with the session started and
  // device started ones.
  uint16_t half_depth = impl_.info().rx_depth / 2;
  for (uint16_t i = 0; i < half_depth; i++) {
    ASSERT_OK(session.SendRx(descriptors[i]));
    ASSERT_OK(WaitRxAvailable());
    ASSERT_EQ(impl_.rx_buffer_count(), i + 1u);
  }
  // Send the rest of the buffers.
  size_t actual;
  ASSERT_OK(
      session.SendRx(descriptors.data() + half_depth, descriptors.size() - half_depth, &actual));
  ASSERT_EQ(actual, descriptors.size() - half_depth);
  ASSERT_OK(WaitRxAvailable());
  ASSERT_EQ(impl_.rx_buffer_count(), impl_.info().rx_depth);

  // Return the maximum number of buffers that we can return without hitting the threshold.
  for (uint16_t i = impl_.info().rx_depth - impl_.info().rx_threshold - 1; i != 0; i--) {
    RxFidlReturnTransaction return_session(&impl_);
    std::unique_ptr buff = impl_.PopRxBuffer();
    buff->SetReturnLength(kReturnBufferSize);
    return_session.Enqueue(std::move(buff), kPort13);
    return_session.Commit();
    // Check that no more buffers are enqueued.
    ASSERT_STATUS(WaitRxAvailable(zx::time::infinite_past()), ZX_ERR_TIMED_OUT)
        << "remaining=" << i;
  }
  // Check again with some time slack for the last buffer.
  ASSERT_STATUS(WaitRxAvailable(zx::deadline_after(zx::msec(10))), ZX_ERR_TIMED_OUT);

  // Return one more buffer to cross the threshold.
  RxFidlReturnTransaction return_session(&impl_);
  std::unique_ptr buff = impl_.PopRxBuffer();
  buff->SetReturnLength(kReturnBufferSize);
  return_session.Enqueue(std::move(buff), kPort13);
  return_session.Commit();
  ASSERT_OK(WaitRxAvailable());
  ASSERT_EQ(impl_.rx_buffer_count(), impl_.info().rx_depth);
}

TEST_F(NetworkDeviceTest, RxQueueIdlesOnPausedSession) {
  ASSERT_OK(CreateDeviceWithPort13());

  struct {
    fbl::Mutex lock;
    std::optional<uint64_t> key __TA_GUARDED(lock);
  } observed_key;

  sync_completion_t completion;

  auto get_next_key = [&observed_key, &completion](zx::duration timeout) -> zx::result<uint64_t> {
    zx_status_t status = sync_completion_wait(&completion, timeout.get());
    fbl::AutoLock l(&observed_key.lock);
    std::optional k = observed_key.key;
    if (status != ZX_OK) {
      // Whenever wait fails, key must not have a value.
      EXPECT_EQ(k, std::nullopt);
      return zx::error(status);
    }
    sync_completion_reset(&completion);
    if (!k.has_value()) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
    uint64_t key = *k;
    observed_key.key.reset();
    return zx::ok(key);
  };

  SetEvtRxQueuePacketHandler([&observed_key, &completion](uint64_t key) {
    fbl::AutoLock l(&observed_key.lock);
    std::optional k = observed_key.key;
    EXPECT_EQ(k, std::nullopt);
    observed_key.key = key;
    sync_completion_signal(&completion);
  });
  auto undo = fit::defer([this]() {
    // Clear event handler so we don't see any of the teardown.
    SetEvtRxQueuePacketHandler(nullptr);
  });

  TestSession session;
  ASSERT_OK(OpenSession(&session));

  {
    zx::result key = get_next_key(zx::duration::infinite());
    ASSERT_OK(key.status_value());
    ASSERT_EQ(key.value(), internal::RxQueue::kSessionSwitchKey);
  }

  session.ResetDescriptor(kDescriptorIndex0);
  // Make the FIFO readable.
  ASSERT_OK(session.SendRx(kDescriptorIndex0));
  // It should not trigger any RxQueue events.
  {
    zx::result key = get_next_key(zx::msec(50));
    ASSERT_TRUE(key.is_error()) << "unexpected key value " << key.value();
    ASSERT_STATUS(key.status_value(), ZX_ERR_TIMED_OUT);
  }

  // Kill the session and check that we see a session switch again.
  ASSERT_OK(session.Close());
  {
    zx::result key = get_next_key(zx::duration::infinite());
    ASSERT_OK(key.status_value());
    ASSERT_EQ(key.value(), internal::RxQueue::kSessionSwitchKey);
  }
}

TEST_F(NetworkDeviceTest, RemovingPortCausesSessionToPause) {
  ASSERT_OK(CreateDeviceWithPort13());
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());

  // Removing the port causes the session to pause, which should cause the data plane to stop.
  fdf::Arena arena('NETD');
  ASSERT_OK(impl_.client().buffer(arena)->RemovePort(kPort13).status());
  ASSERT_OK(WaitStop());
}

TEST_F(NetworkDeviceTest, OnlyReceiveOnSubscribedPorts) {
  ASSERT_OK(CreateDeviceWithPort13());
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  std::array<uint16_t, 2> descriptors = {0, 1};

  for (uint16_t desc : descriptors) {
    buffer_descriptor_t& descriptor = session.ResetDescriptor(desc);
    // Garble descriptor port and salt.
    descriptor.port_id = {
        .base = MAX_PORTS - 1,
        .salt = static_cast<uint8_t>(~(MAX_PORTS - 1)),
    };
  }
  size_t actual;
  ASSERT_OK(session.SendRx(descriptors.data(), descriptors.size(), &actual));
  ASSERT_EQ(actual, descriptors.size());
  ASSERT_OK(WaitRxAvailable());
  ASSERT_EQ(impl_.rx_buffer_count(), descriptors.size());
  RxFidlReturnTransaction return_session(&impl_);
  for (size_t i = 0; i < descriptors.size(); i++) {
    std::unique_ptr rx_space = impl_.PopRxBuffer();
    // Set the port ID to an offset based the index, we should expect the session to only see port
    // 13.
    uint8_t port_id = kPort13 + static_cast<uint8_t>(i);
    // Write some data so the buffer makes it into the session.
    ASSERT_OK(rx_space->WriteData(cpp20::span(&port_id, sizeof(port_id)), impl_.VmoGetter()));
    std::unique_ptr ret = std::make_unique<RxFidlReturn>(std::move(rx_space), port_id);
    return_session.Enqueue(std::move(ret));
  }
  return_session.Commit();
  ASSERT_OK(WaitRxAvailable());
  ASSERT_OK(session.FetchRx(descriptors.data(), descriptors.size(), &actual));
  // Only one of the descriptors makes it back into the session.
  ASSERT_EQ(actual, 1u);
  {
    uint16_t returned = descriptors[0];
    const buffer_descriptor_t& desc = session.descriptor(returned);
    ASSERT_EQ(desc.port_id.base, kPort13);
    ASSERT_EQ(desc.port_id.salt, GetSaltedPortId(kPort13).salt);
  }
  // The unused descriptor comes right back to us.
  ASSERT_EQ(impl_.rx_buffer_count(), 1u);
}

TEST_F(NetworkDeviceTest, SessionsAttachToPort) {
  port13_.SetMac(mac_impl_.Bind(dispatcher().get()));
  ASSERT_OK(CreateDeviceWithPort13());
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  // Just opening a session doesn't attach to port 13.
  ASSERT_STATUS(WaitPortActiveChanged(port13_, zx::deadline_after(zx::msec(20))), ZX_ERR_TIMED_OUT);
  ASSERT_FALSE(port13_.active());

  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitPortActiveChanged(port13_));
  ASSERT_TRUE(port13_.active());

  ASSERT_OK(DetachSessionPort(session, port13_));
  ASSERT_OK(WaitPortActiveChanged(port13_));
  ASSERT_FALSE(port13_.active());

  // Unpause the session once again, then observe that session detaches on destruction.
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitPortActiveChanged(port13_));
  ASSERT_TRUE(port13_.active());

  ASSERT_OK(session.Close());
  ASSERT_OK(WaitPortActiveChanged(port13_));
  ASSERT_FALSE(port13_.active());
}

TEST_F(NetworkDeviceTest, RejectsInvalidPortIds) {
  ASSERT_OK(CreateDeviceWithPort13());
  {
    // Add a port with an invalid ID.
    FakeNetworkPortImpl fake_port;
    ASSERT_EQ(fake_port.AddPortNoWait(MAX_PORTS, impl_dispatcher_.get(), OpenConnection(),
                                      impl_.client()),
              ZX_ERR_INVALID_ARGS);
    ASSERT_FALSE(fake_port.removed());
  }

  {
    // Add a port with a duplicate ID.
    FakeNetworkPortImpl fake_port;
    ASSERT_EQ(
        fake_port.AddPortNoWait(kPort13, impl_dispatcher_.get(), OpenConnection(), impl_.client()),
        ZX_ERR_ALREADY_EXISTS);
    ASSERT_FALSE(fake_port.removed());
  }
}

TEST_F(NetworkDeviceTest, TxBadPorts) {
  // Test that attempting tx with bad port values causes the buffer to be
  // returned with an error.
  ASSERT_OK(CreateDeviceWithPort13());
  FakeNetworkPortImpl port5;
  ASSERT_OK(port5.AddPort(5, impl_dispatcher_.get(), OpenConnection(), impl_.client()));
  auto cleanup = fit::defer([&port5]() { port5.RemoveSync(); });

  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());

  const struct {
    const char* name;
    netdev::wire::PortId port_id;
  } test_cases[] = {
      {
          .name = "port doesn't exist",
          .port_id =
              {
                  .base = MAX_PORTS - 1,
              },
      },
      {
          .name = "port not attached",
          .port_id = GetSaltedPortId(port5.id()),
      },
      {
          .name = "bad salt",
          .port_id =
              {
                  .base = kPort13,
                  .salt = static_cast<uint8_t>(GetSaltedPortId(kPort13).salt + 1),
              },
      },
  };

  for (const auto& t : test_cases) {
    SCOPED_TRACE(t.name);
    constexpr uint16_t kDesc = 0;
    buffer_descriptor_t& desc = session.ResetDescriptor(kDesc);
    desc.port_id = {
        .base = t.port_id.base,
        .salt = t.port_id.salt,
    };
    ASSERT_OK(session.SendTx(kDesc));
    // Should be returned with an error.
    zx_signals_t observed;
    ASSERT_OK(session.tx_fifo().wait_one(ZX_FIFO_READABLE | ZX_FIFO_PEER_CLOSED,
                                         zx::time::infinite(), &observed));
    ASSERT_EQ(observed & (ZX_FIFO_READABLE | ZX_FIFO_PEER_CLOSED), ZX_FIFO_READABLE);
    uint16_t read_desc = 0xFFFF;
    ASSERT_OK(session.FetchTx(&read_desc));
    ASSERT_EQ(read_desc, kDesc);
    ASSERT_EQ(desc.return_flags,
              static_cast<uint32_t>(netdev::wire::TxReturnFlags::kTxRetError |
                                    netdev::wire::TxReturnFlags::kTxRetNotAvailable));
  }
}

TEST_F(NetworkDeviceTest, RxCrossSessionChaining) {
  // Test that attempting to chain Rx buffers that originated from different sessions will cause
  // the frame to be dropped and that no descriptors will be swallowed.
  ASSERT_OK(CreateDeviceWithPort13());
  TestSession session_a;
  ASSERT_OK(OpenSession(&session_a));
  ASSERT_OK(AttachSessionPort(session_a, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());
  // Send a single descriptor to the device and wait for it to be available.
  session_a.ResetDescriptor(kDescriptorIndex0);
  ASSERT_OK(session_a.SendRx(kDescriptorIndex0));
  ASSERT_OK(WaitRxAvailable());
  std::unique_ptr buffer_a = impl_.PopRxBuffer();
  ASSERT_TRUE(buffer_a);
  // Start a second session.
  TestSession session_b;
  ASSERT_OK(OpenSession(&session_b));
  ASSERT_OK(AttachSessionPort(session_b, port13_));
  ASSERT_OK(WaitSessionStarted());
  session_b.ResetDescriptor(kDescriptorIndex0);
  ASSERT_OK(session_b.SendRx(kDescriptorIndex0));

  // Close session A, it should no longer be primary. Then we should receive the rx buffer from
  // session B.
  ASSERT_OK(session_a.Close());
  ASSERT_OK(WaitRxAvailable());
  // We still hold buffer from Session A, it can't be fully closed yet.
  ASSERT_STATUS(session_a.WaitClosed(zx::time::infinite_past()), ZX_ERR_TIMED_OUT);

  std::unique_ptr buffer_b = impl_.PopRxBuffer();
  ASSERT_TRUE(buffer_b);
  const fuchsia_hardware_network_driver::wire::RxSpaceBuffer space_b = buffer_b->space();

  // Space from each buffer must've come from different VMOs.
  ASSERT_NE(buffer_a->space().region.vmo, buffer_b->space().region.vmo);
  // Return both buffers as a single chained rx frame.
  buffer_a->return_part().length = 0xdead;
  buffer_b->return_part().length = 0xbeef;
  auto ret = std::make_unique<RxFidlReturn>();
  ret->PushPart(std::move(buffer_a));
  ret->PushPart(std::move(buffer_b));
  {
    RxFidlReturnTransaction transaction(&impl_);
    transaction.Enqueue(std::move(ret));
    libsync::Completion* completion = nullptr;
    SetEvtRxQueuePacketHandler(CreateTriggerRxHandler(&completion));
    transaction.Commit();
    ASSERT_OK(completion->Wait(TEST_DEADLINE));
    SetEvtRxQueuePacketHandler(nullptr);
  }

  // By committing the transaction, the expectation is:
  // - Session A must've stopped because all its buffers have been returned.
  // - Session B must not have received any buffers through the FIFO because the frame must be
  // discarded.
  // - Buffer B must come back to the available buffer queue because it Session B is still valid and
  // the frame was discarded.
  ASSERT_OK(session_a.WaitClosed(zx::time::infinite()));
  {
    uint16_t descriptor = 0xFFFF;
    ASSERT_STATUS(session_b.FetchRx(&descriptor), ZX_ERR_SHOULD_WAIT)
        << "descriptor=" << descriptor;
  }
  ASSERT_OK(WaitRxAvailable());
  std::unique_ptr buffer_b_again = impl_.PopRxBuffer();
  ASSERT_TRUE(buffer_b_again);
  const fuchsia_hardware_network_driver::wire::RxSpaceBuffer& space = buffer_b_again->space();
  EXPECT_EQ(space.region.vmo, space_b.region.vmo);
  EXPECT_EQ(space.region.offset, space_b.region.offset);
  EXPECT_EQ(space.region.length, space_b.region.length);
  {
    RxFidlReturnTransaction transaction(&impl_);
    transaction.Enqueue(std::move(buffer_b_again), kPort13);
    transaction.Commit();
  }
}

TEST_F(NetworkDeviceTest, SessionRejectsChainedRxSpace) {
  // Tests that sessions do not accept chained descriptors on the Rx FIFO.
  ASSERT_OK(CreateDeviceWithPort13());
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  session.ResetDescriptor(kDescriptorIndex1);
  {
    buffer_descriptor_t& desc = session.ResetDescriptor(kDescriptorIndex0);
    desc.chain_length = 1;
    desc.nxt = 1;
  }
  ASSERT_OK(session.SendRx(kDescriptorIndex0));
  // Session will be closed because of bad descriptor.
  ASSERT_OK(session.WaitClosed(zx::time::infinite()));
}

enum class BufferReturnMethod {
  NoReturn,
  ManualReturn,
  ImmediateReturn,
};

using RxTxBufferReturnParameters = std::tuple<RxTxSwitch, BufferReturnMethod, bool>;

const std::string rxTxBufferReturnTestToString(
    const ::testing::TestParamInfo<RxTxBufferReturnParameters>& info) {
  std::stringstream ss;
  auto [rxtx, return_method, auto_stop] = info.param;
  ss << rxTxSwitchToString(rxtx);
  switch (return_method) {
    case BufferReturnMethod::NoReturn:
      ss << "NoReturn";
      break;
    case BufferReturnMethod::ManualReturn:
      ss << "ManualReturn";
      break;
    case BufferReturnMethod::ImmediateReturn:
      ss << "ImmediateReturn";
      break;
  }
  if (auto_stop) {
    ss << "AutoStop";
  } else {
    ss << "NoAutoStop";
  }
  return ss.str();
}

class RxTxBufferReturnTest : public NetworkDeviceTest,
                             public ::testing::WithParamInterface<RxTxBufferReturnParameters> {};

TEST_P(RxTxBufferReturnTest, TestRaceFramesWithDeviceStop) {
  // Test that racing a closing session with data on the Tx FIFO will do the right thing:
  // - No buffers referencing old VMO IDs remain.
  // - The device is stopped appropriately.
  // - VMOs are cleaned up.
  //
  // Some correctness assertions exercised here are part of the test fixtures and enforce correct
  // contract:
  // - NetworkDeviceImplStart and NetworkDeviceImplStop can't be called when device is already in
  // that state.
  ASSERT_OK(CreateDeviceWithPort13());
  const netdev::wire::PortId port13_id = GetSaltedPortId(kPort13);

  auto [rxtx, return_method, auto_stop] = GetParam();
  impl_.set_auto_stop(auto_stop);

  // Run the test multiple times to increase chance of reproducing race in a single run.
  constexpr uint16_t kIterations = 10;
  for (uint16_t i = 0; i < kIterations; i++) {
    TestSession session;
    ASSERT_OK(OpenSession(&session));
    ASSERT_OK(AttachSessionPort(session, port13_));
    ASSERT_OK(WaitStart());
    buffer_descriptor_t& desc = session.ResetDescriptor(i);
    desc.port_id = {
        .base = port13_id.base,
        .salt = port13_id.salt,
    };

    fit::function<void()> manual_return;
    switch (rxtx) {
      case RxTxSwitch::Rx:
        impl_.set_immediate_return_rx(return_method == BufferReturnMethod::ImmediateReturn);
        ASSERT_OK(session.SendRx(i));
        if (return_method == BufferReturnMethod::ManualReturn) {
          ASSERT_OK(WaitRxAvailable());
          std::unique_ptr buffer = impl_.PopRxBuffer();
          buffer->return_part().length = kDefaultBufferLength;
          ASSERT_FALSE(impl_.PopRxBuffer());
          manual_return = [this, buffer = std::move(buffer)]() mutable {
            RxFidlReturnTransaction transact(&impl_);
            transact.Enqueue(std::move(buffer), kPort13);
            transact.Commit();
          };
        }
        break;
      case RxTxSwitch::Tx:
        impl_.set_immediate_return_tx(return_method == BufferReturnMethod::ImmediateReturn);
        ASSERT_OK(session.SendTx(i));
        if (return_method == BufferReturnMethod::ManualReturn) {
          ASSERT_OK(WaitTx());
          std::unique_ptr buffer = impl_.PopTxBuffer();
          buffer->set_status(ZX_OK);
          ASSERT_FALSE(impl_.PopTxBuffer());
          manual_return = [this, buffer = std::move(buffer)]() mutable {
            TxFidlReturnTransaction transact(&impl_);
            transact.Enqueue(std::move(buffer));
            transact.Commit();
          };
        }
        break;
    }
    session.Close();
    if (manual_return) {
      manual_return();
    }
    ASSERT_OK(WaitStop());
    if (!auto_stop) {
      ASSERT_TRUE(impl_.TriggerStop());
    }

    for (;;) {
      zx_wait_item_t items[] = {
          {
              .handle = session.channel().get(),
              .waitfor = ZX_CHANNEL_PEER_CLOSED,
          },
          {
              .handle = impl_.events().get(),
              .waitfor = kEventTx | kEventRxAvailable,
          },
      };
      auto& [session_wait, events_wait] = items;
      ASSERT_OK(zx_object_wait_many(items, std::size(items), TEST_DEADLINE.get()));
      // Here's where we observe and assert on our races. We're waiting for the session to close,
      // but we're racing with rx buffers becoming available again and the session teardown itself.
      if (events_wait.pending & kEventRxAvailable) {
        ASSERT_OK(impl_.events().signal(kEventRxAvailable, 0));
        // If new rx buffers came back to us, the session must not have been closed.
        ASSERT_FALSE(session_wait.pending & ZX_CHANNEL_PEER_CLOSED);
        RxFidlReturnTransaction return_rx(&impl_);
        for (std::unique_ptr buffer = impl_.PopRxBuffer(); buffer; buffer = impl_.PopRxBuffer()) {
          buffer->return_part().length = 0;
          return_rx.Enqueue(std::move(buffer), kPort13);
        }
        return_rx.Commit();
      }

      // When no returns and no auto stopping we may have the pending tx frame that hasn't been
      // returned yet.
      if (return_method == BufferReturnMethod::NoReturn && !auto_stop) {
        if (events_wait.pending & kEventTx) {
          ASSERT_OK(impl_.events().signal(kEventTx, 0));
          // If we still have pending tx buffers then the session must not have been closed.
          ASSERT_FALSE(session_wait.pending & ZX_CHANNEL_PEER_CLOSED);
          TxFidlReturnTransaction return_tx(&impl_);
          for (std::unique_ptr buffer = impl_.PopTxBuffer(); buffer; buffer = impl_.PopTxBuffer()) {
            buffer->set_status(ZX_ERR_UNAVAILABLE);
            return_tx.Enqueue(std::move(buffer));
          }
          return_tx.Commit();
        }
      } else {
        ASSERT_FALSE(events_wait.pending & kEventTx);
      }

      if (session_wait.pending & ZX_CHANNEL_PEER_CLOSED) {
        ASSERT_FALSE(events_wait.pending & kEventTx);
        ASSERT_FALSE(events_wait.pending & kEventRxAvailable);
        break;
      }
    }

    impl_.WaitReleased();
  }
}

INSTANTIATE_TEST_SUITE_P(NetworkDeviceTest, RxTxBufferReturnTest,
                         ::testing::Combine(::testing::Values(RxTxSwitch::Rx, RxTxSwitch::Tx),
                                            ::testing::Values(BufferReturnMethod::NoReturn,
                                                              BufferReturnMethod::ManualReturn,
                                                              BufferReturnMethod::ImmediateReturn),
                                            ::testing::Bool()),
                         rxTxBufferReturnTestToString);

TEST_F(NetworkDeviceTest, PortGetInfo) {
  // Test Port.GetInfo FIDL implementation.
  ASSERT_OK(CreateDeviceWithPort13());
  zx::result port = OpenPort(kPort13);
  ASSERT_OK(port.status_value());
  fidl::WireResult result = port->GetInfo();
  ASSERT_OK(result.status());
  const netdev::wire::PortInfo& port_info = result.value().info;
  const PortInfo& impl_info = port13_.port_info();
  ASSERT_TRUE(port_info.has_id());
  const netdev::wire::PortId& port_id = port_info.id();
  EXPECT_EQ(port_id.base, kPort13);
  EXPECT_EQ(port_id.salt, GetSaltedPortId(kPort13).salt);
  ASSERT_TRUE(port_info.has_base_info());
  const netdev::wire::PortBaseInfo& base_info = port_info.base_info();
  ASSERT_TRUE(base_info.has_port_class());
  EXPECT_EQ(base_info.port_class(),
            static_cast<netdev::wire::PortClass>(port13_.port_info().port_class));
  ASSERT_TRUE(base_info.has_rx_types());
  EXPECT_EQ(base_info.rx_types().count(), impl_info.rx_types.size());
  for (size_t i = 0; i < base_info.rx_types().count(); i++) {
    EXPECT_EQ(base_info.rx_types()[i], static_cast<netdev::wire::FrameType>(impl_info.rx_types[i]));
  }
  ASSERT_TRUE(base_info.has_tx_types());
  EXPECT_EQ(base_info.tx_types().count(), impl_info.tx_types.size());
  for (size_t i = 0; i < base_info.tx_types().count(); i++) {
    EXPECT_EQ(base_info.tx_types()[i].type,
              static_cast<netdev::wire::FrameType>(impl_info.tx_types[i].type));
    EXPECT_EQ(base_info.tx_types()[i].features, impl_info.tx_types[i].features);
    EXPECT_EQ(base_info.tx_types()[i].supported_flags,
              static_cast<netdev::wire::TxFlags>(impl_info.tx_types[i].supported_flags));
  }
}

TEST_F(NetworkDeviceTest, PortGetStatus) {
  // Test Port.GetStatus FIDL implementation.
  ASSERT_OK(CreateDeviceWithPort13());
  zx::result port = OpenPort(kPort13);
  ASSERT_OK(port.status_value());
  constexpr struct {
    const char* name;
    PortStatus status;
  } kTests[] = {
      {
          .name = "offline-1280",
          .status = {.mtu = 1280, .flags = netdev::wire::StatusFlags()},
      },
      {
          .name = "online-1500",
          .status =
              {
                  .mtu = 1500,
                  .flags = netdev::wire::StatusFlags::kOnline,
              },
      },
  };
  for (auto& t : kTests) {
    SCOPED_TRACE(t.name);
    port13_.SetStatus(t.status);
    fidl::WireResult result = port->GetStatus();
    ASSERT_OK(result.status());
    const netdev::wire::PortStatus& status = result.value().status;
    ASSERT_TRUE(status.has_mtu());
    ASSERT_EQ(status.mtu(), port13_.status().mtu);
    ASSERT_TRUE(status.has_flags());
    ASSERT_EQ(status.flags(), static_cast<netdev::wire::StatusFlags>(port13_.status().flags));
  }
}

TEST_F(NetworkDeviceTest, PortGetMac) {
  port13_.SetMac(mac_impl_.Bind(dispatcher().get()));
  ASSERT_OK(CreateDeviceWithPort13());
  zx::result port = OpenPort(kPort13);
  ASSERT_OK(port.status_value());
  auto [client_end, server_end] = fidl::Endpoints<netdev::MacAddressing>::Create();
  ASSERT_OK(port->GetMac(std::move(server_end)).status());
  fidl::WireSyncClient mac{std::move(client_end)};
  fidl::WireResult result = mac->GetUnicastAddress();
  ASSERT_OK(result.status());
  fuchsia_net::wire::MacAddress& addr = result.value().address;
  const auto& octets = mac_impl_.mac().octets;
  EXPECT_TRUE(std::equal(addr.octets.begin(), addr.octets.end(), octets.begin()));
}

TEST_F(NetworkDeviceTest, PortGetMacFails) {
  // Test Port.GetMac FIDL implementation closes the request when port doesn't support mac
  // addressing.
  ASSERT_OK(CreateDeviceWithPort13());
  zx::result port = OpenPort(kPort13);
  ASSERT_OK(port.status_value());
  auto [client_end, server_end] = fidl::Endpoints<netdev::MacAddressing>::Create();
  ASSERT_OK(port->GetMac(std::move(server_end)).status());
  zx::result epitaph = WaitClosedAndReadEpitaph(client_end.channel());
  ASSERT_OK(epitaph.status_value());
  ASSERT_STATUS(epitaph.value(), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(NetworkDeviceTest, NonExistentPort) {
  // Test network device and session operation on non existent ports.
  ASSERT_OK(CreateDeviceWithPort13());
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  const struct {
    netdev::wire::PortId port_id;
    const char* name;
    zx_status_t session_error;
  } kTests[] = {
      {
          .port_id =
              {
                  .base = kPort13 + 1,
              },
          .name = "port doesn't exist",
          .session_error = ZX_ERR_NOT_FOUND,
      },
      {
          .port_id =
              {
                  .base = MAX_PORTS + 20,
              },
          .name = "out of range port ID",
          .session_error = ZX_ERR_INVALID_ARGS,
      },
      {
          .port_id =
              {
                  .base = kPort13,
                  .salt = static_cast<uint8_t>(GetSaltedPortId(kPort13).salt + 1),
              },
          .name = "bad salt",
          .session_error = ZX_ERR_NOT_FOUND,
      },
  };
  for (const auto& t : kTests) {
    SCOPED_TRACE(t.name);
    zx::result port = OpenPort(t.port_id);
    ASSERT_OK(port.status_value());
    zx::result epitaph = WaitClosedAndReadEpitaph(port.value().client_end().channel());
    ASSERT_OK(epitaph.status_value());
    ASSERT_STATUS(epitaph.value(), ZX_ERR_NOT_FOUND);
    ASSERT_STATUS(session.AttachPort(t.port_id, {}), t.session_error);
  }
}

TEST_F(NetworkDeviceTest, MultiplePortsAndSessions) {
  // Test that a device with multiple ports and sessions behaves as expected in regards to frame
  // filtering.
  ASSERT_OK(CreateDevice());

  constexpr uint8_t kPortCount = 2;
  std::array<FakeNetworkPortImpl, kPortCount> ports;
  for (uint8_t i = 0; i < kPortCount; i++) {
    ASSERT_OK(ports[i].AddPort(i + 1, impl_dispatcher_.get(), OpenConnection(), impl_.client()));
  }
  auto remove_ports = fit::defer([&ports]() {
    for (auto& port : ports) {
      port.RemoveSync();
    }
  });

  struct {
    TestSession session;
    const char* const name;
    const netdev::wire::SessionFlags flags;
    const cpp20::span<FakeNetworkPortImpl> attach_ports;
  } sessions[] = {
      {
          .name = "primary first port",
          .flags = netdev::wire::SessionFlags::kPrimary,
          .attach_ports = cpp20::span(ports.begin(), 1),
      },
      {
          .name = "primary both ports",
          .flags = netdev::wire::SessionFlags::kPrimary,
          .attach_ports = cpp20::span(ports.begin(), ports.end()),
      },
      {
          .name = "nonprimary first port",
          .attach_ports = cpp20::span(ports.begin(), 1),
      },
      {
          .name = "listen second port",
          .flags = netdev::wire::SessionFlags::kListenTx,
          .attach_ports = cpp20::span(ports.begin() + 1, 1),
      },
  };

  const std::array<uint16_t, kPortCount> descriptors = {0, 1};
  for (auto& s : sessions) {
    SCOPED_TRACE(s.name);
    ASSERT_OK(OpenSession(&s.session, s.flags));
    for (auto& port : s.attach_ports) {
      ASSERT_OK(AttachSessionPort(s.session, port));
    }
    for (uint16_t desc : descriptors) {
      buffer_descriptor_t& descriptor = s.session.ResetDescriptor(desc);
      // Garble descriptor port and salt.
      descriptor.port_id = {
          .base = MAX_PORTS - 1,
          .salt = static_cast<uint8_t>(~(MAX_PORTS - 1)),
      };
    }
    size_t actual;
    ASSERT_OK(s.session.SendRx(descriptors.data(), descriptors.size(), &actual));
    ASSERT_EQ(actual, descriptors.size());
  }
  ASSERT_OK(WaitStart());
  ASSERT_OK(WaitRxAvailable());
  ASSERT_EQ(impl_.rx_buffer_count(), descriptors.size());

  // Receive one buffer on each of the ports we created.
  RxFidlReturnTransaction return_session(&impl_);
  for (auto& port : ports) {
    SCOPED_TRACE(port.id());
    std::unique_ptr rx_space = impl_.PopRxBuffer();
    uint8_t port_id = port.id();
    // Write some data so the buffer makes it into the session.
    ASSERT_OK(rx_space->WriteData(cpp20::span(&port_id, sizeof(port_id)), impl_.VmoGetter()));
    std::unique_ptr ret = std::make_unique<RxFidlReturn>(std::move(rx_space), port_id);
    return_session.Enqueue(std::move(ret));
  }
  return_session.Commit();
  ASSERT_OK(WaitRxAvailable());

  // Expect the appropriate buffers to be returned to all sessions.
  for (auto& s : sessions) {
    SCOPED_TRACE(s.name);
    std::array<uint16_t, kPortCount> returned_descriptors;
    size_t actual;
    ASSERT_OK(s.session.FetchRx(returned_descriptors.data(), returned_descriptors.size(), &actual));
    ASSERT_EQ(actual, s.attach_ports.size());

    auto desc_iter = returned_descriptors.begin();
    for (auto& port : s.attach_ports) {
      SCOPED_TRACE(port.id());
      const buffer_descriptor_t& desc = s.session.descriptor(*desc_iter++);
      ASSERT_EQ(desc.port_id.base, port.id());
      ASSERT_EQ(desc.port_id.salt, GetSaltedPortId(port.id()).salt);
    }
  }
}

TEST_F(NetworkDeviceTest, ListenSessionPortFiltering) {
  // Tests that a listening session performs port filtering on looped back tx frames.
  ASSERT_OK(CreateDevice());
  constexpr uint8_t kPortCount = 2;
  std::array<FakeNetworkPortImpl, kPortCount> ports;
  for (uint8_t i = 0; i < static_cast<uint8_t>(ports.size()); i++) {
    ASSERT_OK(ports[i].AddPort(i + 1, impl_dispatcher_.get(), OpenConnection(), impl_.client()));
  }
  auto remove_ports = fit::defer([&ports]() {
    for (auto& port : ports) {
      port.RemoveSync();
    }
  });

  TestSession primary_session;
  ASSERT_OK(OpenSession(&primary_session));
  for (auto& port : ports) {
    ASSERT_OK(AttachSessionPort(primary_session, port));
  }
  TestSession listen_session;
  ASSERT_OK(OpenSession(&listen_session, netdev::wire::SessionFlags::kListenTx));
  // Listening session only attaches to the first port.
  ASSERT_OK(AttachSessionPort(listen_session, ports[0]));

  // Prepare descriptors on the listening session.
  for (uint16_t i = 0; i < static_cast<uint16_t>(ports.size()); i++) {
    listen_session.ResetDescriptor(i);
    ASSERT_OK(listen_session.SendRx(i));
  }
  // Send one frame on each port on the primary session.
  {
    std::array<uint16_t, kPortCount> descriptors = {0, 1};
    for (uint8_t i = 0; i < kPortCount; i++) {
      buffer_descriptor_t& desc = primary_session.ResetDescriptor(descriptors[i]);
      netdev::wire::PortId id = GetSaltedPortId(ports[i].id());
      desc.port_id = {
          .base = id.base,
          .salt = id.salt,
      };
    }
    size_t actual;
    ASSERT_OK(primary_session.SendTx(descriptors.data(), descriptors.size(), &actual));
    ASSERT_EQ(actual, descriptors.size());
  }
  ASSERT_OK(WaitTx());

  // Observe the listening session only receive for the port it attached to.
  uint16_t desc;
  ASSERT_OK(listen_session.FetchRx(&desc));

  const buffer_descriptor_t& buffer_desc = listen_session.descriptor(desc);
  const netdev::wire::PortId id = GetSaltedPortId(ports[0].id());
  ASSERT_EQ(buffer_desc.port_id.base, id.base);
  ASSERT_EQ(buffer_desc.port_id.salt, id.salt);
  ASSERT_STATUS(listen_session.FetchRx(&desc), ZX_ERR_SHOULD_WAIT);
}

TEST_F(NetworkDeviceTest, PortWatcher) {
  // Test Port Watchers.
  auto endpoints = fidl::Endpoints<netdev::PortWatcher>::Create();

  struct PortEvent {
    netdev::wire::DevicePortEvent::Tag which;
    std::optional<netdev::wire::PortId> port_id;
  };

  auto watch_next = [watcher = fidl::WireSyncClient(std::move(endpoints.client))]() mutable {
    return std::async([&watcher]() -> zx::result<PortEvent> {
      fidl::WireResult watch = watcher->Watch();
      if (!watch.ok()) {
        return zx::error(watch.status());
      }
      netdev::wire::DevicePortEvent& e = watch.value().event;
      PortEvent event = {.which = e.Which()};
      switch (e.Which()) {
        case netdev::wire::DevicePortEvent::Tag::kIdle:
          break;
        case netdev::wire::DevicePortEvent::Tag::kExisting:
          event.port_id = e.existing();
          break;
        case netdev::wire::DevicePortEvent::Tag::kAdded:
          event.port_id = e.added();
          break;
        case netdev::wire::DevicePortEvent::Tag::kRemoved:
          event.port_id = e.removed();
          break;
      }
      return zx::ok(std::move(event));
    });
  };

  auto expect_event = [](std::future<zx::result<PortEvent>> fut, PortEvent expect) {
    ASSERT_TRUE(fut.valid());
    fut.wait();
    const zx::result<PortEvent>& maybe_event = fut.get();
    ASSERT_OK(maybe_event.status_value());
    const PortEvent& e = maybe_event.value();
    ASSERT_EQ(e.which, expect.which);
    if (expect.port_id.has_value()) {
      ASSERT_TRUE(e.port_id.has_value());
      ASSERT_EQ(e.port_id.value().base, expect.port_id.value().base);
      ASSERT_EQ(e.port_id.value().salt, expect.port_id.value().salt);
    } else {
      ASSERT_FALSE(e.port_id.has_value());
    }
  };
  auto expect_blocked = [](std::future<zx::result<PortEvent>>& fut) {
    ASSERT_TRUE(fut.valid());
    ASSERT_EQ(fut.wait_for(std::chrono::milliseconds(10)), std::future_status::timeout);
  };

  ASSERT_OK(CreateDeviceWithPort13());
  netdev::wire::PortId salted_id = GetSaltedPortId(kPort13);
  fidl::WireSyncClient device = OpenConnection();
  ASSERT_OK(device->GetPortWatcher(std::move(endpoints.server)).status());

  // Should list port 13 on creation.
  ASSERT_NO_FATAL_FAILURE(
      expect_event(watch_next(), {
                                     .which = netdev::wire::DevicePortEvent::Tag::kExisting,
                                     .port_id = salted_id,
                                 }));
  ASSERT_NO_FATAL_FAILURE(
      expect_event(watch_next(), {
                                     .which = netdev::wire::DevicePortEvent::Tag::kIdle,
                                 }));

  std::future fut = watch_next();
  ASSERT_NO_FATAL_FAILURE(expect_blocked(fut));

  // Add a port and observe a new added event once.
  constexpr uint8_t kOtherPortId = 1;
  {
    FakeNetworkPortImpl port;
    ASSERT_OK(port.AddPort(kOtherPortId, impl_dispatcher_.get(), OpenConnection(), impl_.client()));
    netdev::wire::PortId other_salted_id = GetSaltedPortId(kOtherPortId);
    auto remove_port = fit::defer([&port]() { port.RemoveSync(); });
    ASSERT_NO_FATAL_FAILURE(
        expect_event(std::move(fut), {
                                         .which = netdev::wire::DevicePortEvent::Tag::kAdded,
                                         .port_id = other_salted_id,
                                     }));

    fut = watch_next();
    ASSERT_NO_FATAL_FAILURE(expect_blocked(fut));
    remove_port.call();
    ASSERT_NO_FATAL_FAILURE(
        expect_event(std::move(fut), {
                                         .which = netdev::wire::DevicePortEvent::Tag::kRemoved,
                                         .port_id = other_salted_id,
                                     }));
    fut = watch_next();
    ASSERT_NO_FATAL_FAILURE(expect_blocked(fut));
  }

  // Add and remove ports with the same ID without calling watch to prove events are being enqueued.
  std::array<netdev::wire::PortId, 3> install_rounds;

  for (auto& port_id : install_rounds) {
    FakeNetworkPortImpl port;
    ASSERT_OK(port.AddPort(kOtherPortId, impl_dispatcher_.get(), OpenConnection(), impl_.client()));
    port_id = GetSaltedPortId(kOtherPortId);
    port.RemoveSync();
  }

  for (auto& port_id : install_rounds) {
    SCOPED_TRACE(port_id.base);
    ASSERT_NO_FATAL_FAILURE(
        expect_event(std::move(fut), {
                                         .which = netdev::wire::DevicePortEvent::Tag::kAdded,
                                         .port_id = port_id,
                                     }));
    ASSERT_NO_FATAL_FAILURE(
        expect_event(watch_next(), {
                                       .which = netdev::wire::DevicePortEvent::Tag::kRemoved,
                                       .port_id = port_id,
                                   }));
    fut = watch_next();
  }
  ASSERT_NO_FATAL_FAILURE(expect_blocked(fut));

  // Discard device, watcher should close and thread should end.
  DiscardDeviceSync();
  fut.wait();
  ASSERT_STATUS(fut.get().status_value(), ZX_ERR_PEER_CLOSED);
}

TEST_F(NetworkDeviceTest, PortWatcherEnforcesQueueLimit) {
  // Tests that port watchers close the channel when too many events are enqueued.
  ASSERT_OK(CreateDevice());
  auto endpoints = fidl::Endpoints<netdev::PortWatcher>::Create();
  fidl::WireSyncClient device = OpenConnection();
  ASSERT_OK(device->GetPortWatcher(std::move(endpoints.server)).status());
  fidl::ClientEnd watcher = std::move(endpoints.client);
  // Call watch once to observe the idle event and ensure no races between watcher binding and
  // adding ports will happen.
  fidl::WireResult result = fidl::WireCall(watcher)->Watch();
  ASSERT_OK(result.status());
  ASSERT_EQ(result.value().event.Which(), netdev::wire::DevicePortEvent::Tag::kIdle);

  // Add and remove ports until we've used up all the event queue.
  std::unique_ptr<FakeNetworkPortImpl> port;
  auto remove_port = fit::defer([&port]() {
    if (port) {
      port->RemoveSync();
    }
  });
  for (size_t event_count = 0; event_count <= internal::PortWatcher::kMaximumQueuedEvents;
       event_count++) {
    zx_signals_t pending = 0;
    ASSERT_STATUS(watcher.channel().wait_one(ZX_CHANNEL_PEER_CLOSED | ZX_CHANNEL_READABLE,
                                             zx::time::infinite_past(), &pending),
                  ZX_ERR_TIMED_OUT)
        << pending;
    // Alternate between creating or destroying a port.
    if (port) {
      port->RemoveSync();
      port = nullptr;
    } else {
      port = std::make_unique<FakeNetworkPortImpl>();
      ASSERT_OK(port->AddPort((event_count / 2) % MAX_PORTS, impl_dispatcher_.get(),
                              OpenConnection(), impl_.client()));
    }
  }
  zx::result status = WaitClosedAndReadEpitaph(watcher.channel());
  ASSERT_OK(status.status_value());
  ASSERT_STATUS(status.value(), ZX_ERR_CANCELED);
}

enum class DescriptorSource {
  PrimarySessionRx,
  SecondarySessionRx,
  ListenSessionRx,
  Tx,
  TxChain,
};

class BadDescriptorTest : public NetworkDeviceTest,
                          public ::testing::WithParamInterface<DescriptorSource> {};

const std::string badDescriptorTestToString(
    const ::testing::TestParamInfo<DescriptorSource>& info) {
  switch (info.param) {
    case DescriptorSource::PrimarySessionRx:
      return "PrimarySessionRx";
    case DescriptorSource::SecondarySessionRx:
      return "SecondarySessionRx";
    case DescriptorSource::ListenSessionRx:
      return "ListenSessionRx";
    case DescriptorSource::Tx:
      return "Tx";
    case DescriptorSource::TxChain:
      return "TxChain";
  }
}

TEST_P(BadDescriptorTest, SessionIsKilledOnBadDescriptor) {
  impl_.set_immediate_return_tx(true);
  ASSERT_OK(CreateDeviceWithPort13());
  const netdev::wire::PortId port13_id = GetSaltedPortId(kPort13);
  TestSession primary;
  TestSession secondary;
  TestSession listen;

  constexpr uint16_t kDescriptorCount = 8;
  constexpr uint16_t kInitialRxDescriptors = kDescriptorCount / 2;
  constexpr uint16_t kGoodTxDescriptor = kDescriptorCount - 1;
  const struct {
    TestSession& session;
    const char* name;
    netdev::wire::SessionFlags flags;
    bool send_bad_rx_descriptor;
  } kSessions[] = {
      {
          .session = primary,
          .name = "primary",
          .flags = netdev::wire::SessionFlags::kPrimary,
          .send_bad_rx_descriptor = GetParam() == DescriptorSource::PrimarySessionRx,
      },
      {
          .session = secondary,
          .name = "secondary",
          .send_bad_rx_descriptor = GetParam() == DescriptorSource::SecondarySessionRx,
      },
      {
          .session = listen,
          .name = "listen",
          .flags = netdev::wire::SessionFlags::kListenTx,
          .send_bad_rx_descriptor = GetParam() == DescriptorSource::ListenSessionRx,
      },
  };
  for (auto& s : kSessions) {
    SCOPED_TRACE(s.name);
    ASSERT_OK(OpenSession(&s.session, s.flags, kDescriptorCount, kDefaultBufferLength, s.name));
    ASSERT_OK(AttachSessionPort(s.session, port13_));
    uint16_t rx_descriptors[kInitialRxDescriptors];
    const uint16_t descriptor_offset = s.send_bad_rx_descriptor ? kDescriptorCount : 0;
    for (uint16_t i = 0; i < kInitialRxDescriptors; i++) {
      s.session.ResetDescriptor(i);
      rx_descriptors[i] = i + descriptor_offset;
    }
    size_t actual;
    ASSERT_OK(s.session.SendRx(rx_descriptors, std::size(rx_descriptors), &actual));
    ASSERT_EQ(actual, std::size(rx_descriptors));
  }

  switch (GetParam()) {
    case DescriptorSource::PrimarySessionRx:
      break;
    case DescriptorSource::SecondarySessionRx: {
      ASSERT_OK(WaitRxAvailable());
      RxFidlReturnTransaction txn(&impl_);
      std::unique_ptr rx_buffer = impl_.PopRxBuffer();
      rx_buffer->SetReturnLength(1);
      txn.Enqueue(std::move(rx_buffer), kPort13);
      txn.Commit();
    } break;
    case DescriptorSource::ListenSessionRx: {
      buffer_descriptor_t& desc = primary.ResetDescriptor(kGoodTxDescriptor);
      desc.port_id = {
          .base = port13_id.base,
          .salt = port13_id.salt,
      };
      ASSERT_OK(primary.SendTx(kGoodTxDescriptor));
    } break;
    case DescriptorSource::Tx:
      ASSERT_OK(primary.SendTx(kDescriptorCount));
      break;
    case DescriptorSource::TxChain: {
      buffer_descriptor_t& desc = primary.ResetDescriptor(kGoodTxDescriptor);
      desc.port_id = {
          .base = port13_id.base,
          .salt = port13_id.salt,
      };
      desc.chain_length = 1;
      desc.nxt = kDescriptorCount;
      ASSERT_OK(primary.SendTx(kGoodTxDescriptor));
    } break;
  }

  TestSession& killed_session = [&primary, &secondary, &listen]() -> TestSession& {
    switch (GetParam()) {
      case DescriptorSource::PrimarySessionRx:
      case DescriptorSource::Tx:
      case DescriptorSource::TxChain:
        return primary;
      case DescriptorSource::SecondarySessionRx:
        return secondary;
      case DescriptorSource::ListenSessionRx:
        return listen;
    }
  }();

  for (auto& s : kSessions) {
    SCOPED_TRACE(s.name);
    if (&s.session == &killed_session) {
      ASSERT_OK(s.session.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, TEST_DEADLINE, nullptr));
    } else {
      zx_signals_t pending = 0;
      ASSERT_STATUS(s.session.channel().wait_one(ZX_CHANNEL_PEER_CLOSED,
                                                 zx::deadline_after(zx::msec(10)), &pending),
                    ZX_ERR_TIMED_OUT)
          << pending;
    }
  }
}

INSTANTIATE_TEST_SUITE_P(NetworkDeviceTest, BadDescriptorTest,
                         ::testing::Values(DescriptorSource::PrimarySessionRx,
                                           DescriptorSource::SecondarySessionRx,
                                           DescriptorSource::ListenSessionRx, DescriptorSource::Tx,
                                           DescriptorSource::TxChain),
                         badDescriptorTestToString);

TEST_F(NetworkDeviceTest, SecondarySessionWithRxOffsetAndChaining) {
  constexpr uint32_t kBufferLength = 32;
  ASSERT_OK(CreateDeviceWithPort13());
  struct {
    TestSession session;
    const char* const name;
    const netdev::wire::SessionFlags flags;
    const uint16_t descriptor_count;
  } sessions[] = {
      {
          .name = "primary",
          .flags = netdev::wire::SessionFlags::kPrimary,
          .descriptor_count = 1,
      },
      {
          .name = "alt_a",
          .descriptor_count = 2,
      },
      {
          .name = "alt_b",
          .descriptor_count = 4,
      },
  };

  struct {
    const uint32_t offset;
    const uint32_t length;
    std::vector<uint8_t> reference_data;
  } buffers[] = {
      {.offset = 0, .length = kBufferLength},
      {.offset = 3, .length = kBufferLength / 4},
      {.offset = kBufferLength / 4, .length = kBufferLength / 2},
  };

  for (auto& s : sessions) {
    SCOPED_TRACE(s.name);
    ASSERT_OK(OpenSession(&s.session, s.flags, kDefaultDescriptorCount, kBufferLength, s.name));
    for (uint16_t desc = 0; desc < std::size(buffers) * s.descriptor_count; desc++) {
      buffer_descriptor_t& d = s.session.ResetDescriptor(desc);
      d.data_length = kBufferLength / s.descriptor_count;
      ASSERT_OK(s.session.SendRx(desc));
    }
    ASSERT_OK(AttachSessionPort(s.session, port13_));
  }
  ASSERT_OK(WaitStart());

  ASSERT_OK(WaitRxAvailable());
  RxFidlReturnTransaction txn(&impl_);
  for (auto& b : buffers) {
    b.reference_data.reserve(b.length);
    for (uint32_t i = 0; i < b.length; i++) {
      b.reference_data.push_back(static_cast<uint8_t>(i ^ b.offset));
    }
    std::unique_ptr rx_space = impl_.PopRxBuffer();
    ASSERT_TRUE(rx_space);
    ASSERT_GE(rx_space->space().region.length, b.length + b.offset);
    rx_space->space().region.offset += b.offset;
    ASSERT_OK(rx_space->WriteData(b.reference_data, impl_.VmoGetter()));
    rx_space->return_part() = {
        .id = rx_space->return_part().id,
        .offset = b.offset,
        .length = b.length,
    };
    txn.Enqueue(std::move(rx_space), kPort13);
  }
  libsync::Completion* completion = nullptr;
  SetEvtRxQueuePacketHandler(CreateTriggerRxHandler(&completion));
  txn.Commit();
  ASSERT_OK(completion->Wait(TEST_DEADLINE));

  SetEvtRxQueuePacketHandler(nullptr);

  for (auto& s : sessions) {
    SCOPED_TRACE(s.name);
    for (auto& b : buffers) {
      std::stringstream ss;
      ss << "offset:" << b.offset << ",length:" << b.length;
      SCOPED_TRACE(ss.str());

      uint16_t desc_idx;
      ASSERT_OK(s.session.FetchRx(&desc_idx));
      {
        buffer_descriptor_t& desc = s.session.descriptor(desc_idx);
        if (s.flags & netdev::wire::SessionFlags::kPrimary) {
          ASSERT_EQ(desc.chain_length, 0);
        } else {
          ASSERT_EQ(desc.chain_length,
                    std::max(static_cast<uint8_t>(b.length * s.descriptor_count / kBufferLength),
                             static_cast<uint8_t>(1)) -
                        1);
        }
      }
      uint8_t received[kBufferLength];
      auto wr_iter = std::begin(received);
      for (;;) {
        buffer_descriptor_t& desc = s.session.descriptor(desc_idx);
        ASSERT_LE(
            static_cast<size_t>(std::distance(std::begin(received), wr_iter)) + desc.data_length,
            std::size(received));
        wr_iter = std::copy_n(s.session.buffer(desc.offset + desc.head_length), desc.data_length,
                              wr_iter);
        if (desc.chain_length == 0) {
          break;
        }
        desc_idx = desc.nxt;
      }
      ASSERT_EQ(static_cast<size_t>(std::distance(std::begin(received), wr_iter)),
                b.reference_data.size());
      ASSERT_EQ(toHexString(cpp20::span(received, b.reference_data.size())),
                toHexString(cpp20::span(b.reference_data.data(), b.reference_data.size())));
    }
  }
}

TEST_F(NetworkDeviceTest, BufferChainingOnListenTx) {
  ASSERT_OK(CreateDeviceWithPort13());
  TestSession primary;
  ASSERT_OK(OpenSession(&primary, netdev::wire::SessionFlags::kPrimary, kDefaultDescriptorCount,
                        kDefaultBufferLength, "primary"));
  ASSERT_OK(AttachSessionPort(primary, port13_));
  TestSession listen;
  ASSERT_OK(OpenSession(&listen, netdev::wire::SessionFlags::kListenTx, kDefaultDescriptorCount,
                        kDefaultBufferLength, "listen"));
  ASSERT_OK(AttachSessionPort(listen, port13_));
  ASSERT_OK(WaitStart());

  constexpr uint32_t kRxDescriptorLen = 30;
  constexpr uint16_t kRxDescriptorCount = 3;
  constexpr uint16_t kTxHeadLen = 10;
  constexpr uint32_t kTxLen = kRxDescriptorLen * kRxDescriptorCount - 4;
  constexpr uint16_t kTxDescriptor = 0;

  for (uint16_t i = 0; i < kRxDescriptorCount; i++) {
    buffer_descriptor_t& desc = listen.ResetDescriptor(i);
    desc.data_length = kRxDescriptorLen;
    ASSERT_OK(listen.SendRx(i));
  }

  buffer_descriptor_t& tx_desc = primary.ResetDescriptor(kTxDescriptor);
  tx_desc.port_id = {
      .base = kPort13,
      .salt = GetSaltedPortId(kPort13).salt,
  };
  tx_desc.data_length = kTxLen;
  tx_desc.head_length = kTxHeadLen;
  uint8_t b = 0;
  cpp20::span tx_data(primary.buffer(tx_desc.offset + kTxHeadLen), kTxLen);
  for (uint8_t& d : tx_data) {
    d = b++;
  }
  ASSERT_OK(primary.SendTx(kTxDescriptor));

  ASSERT_OK(listen.rx_fifo().wait_one(ZX_FIFO_READABLE, TEST_DEADLINE, nullptr));
  uint16_t rx_desc_index;
  ASSERT_OK(listen.FetchRx(&rx_desc_index));

  uint32_t offset = 0;
  uint8_t expect_chain_length = kRxDescriptorCount - 1;
  for (uint16_t i = 0; i < kRxDescriptorCount; i++) {
    SCOPED_TRACE(i);
    buffer_descriptor_t& rx_desc = listen.descriptor(rx_desc_index);
    ASSERT_EQ(rx_desc.chain_length, expect_chain_length--);
    cpp20::span data(listen.buffer(rx_desc.offset), rx_desc.data_length);
    ASSERT_EQ(data.size(), std::min(kRxDescriptorLen, kTxLen - offset));
    ASSERT_EQ(toHexString(cpp20::span(data.begin(), data.size())),
              toHexString(tx_data.subspan(offset, data.size())));
    rx_desc_index = rx_desc.nxt;
    offset += rx_desc.data_length;
  }
  ASSERT_EQ(offset, kTxLen);
}

TEST_F(NetworkDeviceTest, SessionsClosedOnStartFailure) {
  ASSERT_OK(CreateDeviceWithPort13());

  auto assert_no_sessions = [this] {
    auto& device = *static_cast<internal::DeviceInterface*>(device_.get());
    fbl::AutoLock lock(&device.control_lock());
    ASSERT_TRUE(GetDeviceSessionsUnsafe(device).is_empty());
    ASSERT_EQ(GetPrimarySession(device), nullptr);
    ASSERT_FALSE(device.IsDataPlaneOpen());
  };

  impl_.set_auto_start(ZX_ERR_INTERNAL);
  TestSession primary;
  ASSERT_OK(OpenSession(&primary, netdev::wire::SessionFlags::kPrimary, kDefaultDescriptorCount,
                        kDefaultBufferLength, "primary"));
  ASSERT_OK(AttachSessionPort(primary, port13_));
  ASSERT_OK(WaitSessionDied());
  ASSERT_NO_FATAL_FAILURE(assert_no_sessions());

  TestSession secondary;
  ASSERT_OK(OpenSession(&secondary, netdev::wire::SessionFlags::kMask, kDefaultDescriptorCount,
                        kDefaultBufferLength, "secondary"));
  ASSERT_OK(AttachSessionPort(secondary, port13_));
  ASSERT_OK(WaitSessionDied());
  ASSERT_NO_FATAL_FAILURE(assert_no_sessions());

  TestSession tertiary;
  ASSERT_OK(OpenSession(&tertiary, netdev::wire::SessionFlags::kMask, kDefaultDescriptorCount,
                        kDefaultBufferLength, "tertiary"));
  ASSERT_OK(AttachSessionPort(tertiary, port13_));
  ASSERT_OK(WaitSessionDied());
  ASSERT_NO_FATAL_FAILURE(assert_no_sessions());
}

INSTANTIATE_TEST_SUITE_P(NetworkDeviceTest, RxTxParamTest,
                         ::testing::Values(RxTxSwitch::Rx, RxTxSwitch::Tx), rxTxParamTestToString);

TEST_F(NetworkDeviceTest, CanUpdatePortStatusWithinSetActive) {
  // Tests that notifying status changes inline in a port SetActive call doesn't cause a deadlock.
  ASSERT_OK(CreateDeviceWithPort13());
  uint32_t set_active_call_counter = 0;
  port13_.SetOnSetActiveCallback([this, &set_active_call_counter](bool active) {
    port13_.SetOnline(active);
    set_active_call_counter++;
  });

  fidl::ClientEnd<netdev::StatusWatcher> client_end;
  {
    zx::result server_end = fidl::CreateEndpoints(&client_end);
    ASSERT_OK(server_end.status_value());
    zx::result port = OpenPort(kPort13);
    ASSERT_OK(port.status_value());
    constexpr uint32_t kWatcherBuffer = 3;
    ASSERT_OK(port->GetStatusWatcher(std::move(server_end.value()), kWatcherBuffer).status());
  }
  fidl::WireSyncClient watcher{std::move(client_end)};

  {
    fidl::WireResult result = watcher->WatchStatus();
    ASSERT_OK(result.status());
    ASSERT_EQ(result.value().port_status.flags(), netdev::wire::StatusFlags());
  }

  TestSession session;
  ASSERT_OK(OpenSession(&session, netdev::wire::SessionFlags::kPrimary, kDefaultDescriptorCount,
                        kDefaultBufferLength, "primary"));

  // Port goes online on SetActive callback when session attaches.
  {
    ASSERT_OK(AttachSessionPort(session, port13_));
    fidl::WireResult result = watcher->WatchStatus();
    ASSERT_OK(result.status());
    ASSERT_EQ(result.value().port_status.flags(), netdev::wire::StatusFlags::kOnline);
    ASSERT_EQ(set_active_call_counter, 1u);
  }

  // Port goes offline on SetActive callback when session detaches.
  {
    ASSERT_OK(session.DetachPort(GetSaltedPortId(kPort13)));
    fidl::WireResult result = watcher->WatchStatus();
    ASSERT_OK(result.status());
    ASSERT_EQ(result.value().port_status.flags(), netdev::wire::StatusFlags());
    ASSERT_EQ(set_active_call_counter, 2u);
  }
}

// This test guards against a regression where a dangling session would prevent device teardown from
// completing.
TEST_F(NetworkDeviceTest, DeadSessionsDontPreventTeardown) {
  ASSERT_OK(CreateDeviceWithPort13());
  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());
  session.ResetDescriptor(kDescriptorIndex0);
  ASSERT_OK(session.SendRx(kDescriptorIndex0));
  ASSERT_OK(WaitRxAvailable());
  std::unique_ptr buffer = impl_.PopRxBuffer();
  ASSERT_TRUE(buffer);

  // Perform an active session close and wait for stop to be triggered, that puts the session in the
  // dead state.
  ASSERT_OK(session.Close());
  ASSERT_OK(WaitStop(TEST_DEADLINE));
  // The channel still isn't closed because we're holding a buffer.
  ASSERT_STATUS(session.WaitClosed(zx::time::infinite_past()), ZX_ERR_TIMED_OUT);

  // Start a device teardown while holding the buffer, which puts the session in a dead state.
  sync_completion_t completer;
  device_->Teardown([&completer, this]() {
    // Destroy device to prevent test fixture from attempting to tear it down again.
    device_ = nullptr;
    sync_completion_signal(&completer);
  });
  // Teardown isn't called and the session isn't closed yet because we haven't returned the buffer.
  ASSERT_STATUS(session.WaitClosed(zx::time::infinite_past()), ZX_ERR_TIMED_OUT);
  ASSERT_STATUS(sync_completion_wait_deadline(&completer, zx::time::infinite_past().get()),
                ZX_ERR_TIMED_OUT);

  RxFidlReturnTransaction txn(&impl_);
  txn.Enqueue(std::move(buffer), port13_.id());
  txn.Commit();

  // After returning the buffers, the session is closed and teardown completes.
  ASSERT_OK(session.WaitClosed(TEST_DEADLINE));
  ASSERT_OK(sync_completion_wait_deadline(&completer, TEST_DEADLINE.get()));
}

TEST_F(NetworkDeviceTest, CloneDevice) {
  impl_.info().min_rx_buffer_length = 1234;
  ASSERT_OK(CreateDevice());
  fidl::WireSyncClient connection1 = OpenConnection();
  auto [client_end, server_end] = fidl::Endpoints<netdev::Device>::Create();
  ASSERT_OK(connection1->Clone(std::move(server_end)).status());
  fidl::WireSyncClient connection2{std::move(client_end)};
  fidl::WireResult result = connection2->GetInfo();
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().info.has_base_info());
  const auto& base_info = result.value().info.base_info();
  ASSERT_TRUE(base_info.has_min_rx_buffer_length());
  ASSERT_EQ(base_info.min_rx_buffer_length(), impl_.info().min_rx_buffer_length);
}

TEST_F(NetworkDeviceTest, ClonePort) {
  ASSERT_OK(CreateDeviceWithPort13());
  zx::result port = OpenPort(kPort13);
  ASSERT_OK(port.status_value());
  fidl::WireSyncClient connection1 = std::move(port.value());
  auto [client_end, server_end] = fidl::Endpoints<netdev::Port>::Create();
  ASSERT_OK(connection1->Clone(std::move(server_end)).status());
  fidl::WireSyncClient connection2{std::move(client_end)};

  netdev::wire::PortId salted_id1, salted_id2;
  {
    fidl::WireResult result = connection1->GetInfo();
    ASSERT_OK(result.status());
    salted_id1 = result.value().info.id();
  }
  {
    fidl::WireResult result = connection2->GetInfo();
    ASSERT_OK(result.status());
    salted_id2 = result.value().info.id();
  }
  EXPECT_EQ(salted_id1.base, salted_id2.base);
  EXPECT_EQ(salted_id1.salt, salted_id2.salt);
}

TEST_F(NetworkDeviceTest, PortGetDevice) {
  impl_.info().min_rx_buffer_length = 1234;
  ASSERT_OK(CreateDeviceWithPort13());
  zx::result port = OpenPort(kPort13);
  ASSERT_OK(port.status_value());
  fidl::WireSyncClient port_connection = std::move(port.value());
  auto [client_end, server_end] = fidl::Endpoints<netdev::Device>::Create();
  ASSERT_OK(port_connection->GetDevice(std::move(server_end)).status());
  fidl::WireSyncClient device_connection{std::move(client_end)};
  fidl::WireResult result = device_connection->GetInfo();
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().info.has_base_info());
  const auto& base_info = result.value().info.base_info();
  ASSERT_TRUE(base_info.has_min_rx_buffer_length());
  ASSERT_EQ(base_info.min_rx_buffer_length(), impl_.info().min_rx_buffer_length);
}

TEST_F(NetworkDeviceTest, PortIdSaltChangesOnFlap) {
  ASSERT_OK(CreateDevice());
  const uint8_t base_salt = GetSaltedPortId(kPort13).salt + 1;
  for (uint8_t i = 0; i < 5; i++) {
    SCOPED_TRACE(static_cast<uint32_t>(i));
    // Salt will increase monotonically with each round of port addition,
    // removal.
    const uint8_t expect_salt = base_salt + i;
    FakeNetworkPortImpl port;
    ASSERT_OK(port.AddPort(kPort13, impl_dispatcher_.get(), OpenConnection(), impl_.client()));
    // Check internal ID and salt.
    {
      netdev::wire::PortId id = GetSaltedPortId(kPort13);
      EXPECT_EQ(id.base, kPort13);
      EXPECT_EQ(id.salt, expect_salt);
    }
    // Check public API ID and salt.
    {
      zx::result status = OpenPort(kPort13);
      ASSERT_OK(status.status_value());
      fidl::WireSyncClient port = std::move(status.value());
      fidl::WireResult result = port->GetInfo();
      ASSERT_OK(result.status());
      const netdev::wire::PortInfo& info = result.value().info;
      ASSERT_TRUE(info.has_id());
      const netdev::wire::PortId id = info.id();
      EXPECT_EQ(id.base, kPort13);
      EXPECT_EQ(id.salt, expect_salt);
    }
    port.RemoveSync();
  }
}

TEST_F(NetworkDeviceTest, PortGetRxCounters) {
  ASSERT_OK(CreateDeviceWithPort13());

  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());

  zx::result port = OpenPort(kPort13);
  ASSERT_OK(port.status_value());
  fidl::WireSyncClient port_connection = std::move(port.value());

  constexpr uint16_t kDescriptors[] = {kDescriptorIndex0, kDescriptorIndex1, kDescriptorIndex2,
                                       kDescriptorIndex3};
  for (const uint16_t desc : kDescriptors) {
    session.ResetDescriptor(desc);
  }
  size_t actual;
  ASSERT_OK(session.SendRx(kDescriptors, std::size(kDescriptors), &actual));
  ASSERT_EQ(actual, std::size(kDescriptors));
  ASSERT_OK(WaitRxAvailable());

  constexpr uint32_t kReturnLength = 17;

  auto prepare_return_buffer = [this]() -> std::unique_ptr<RxFidlReturn> {
    std::unique_ptr buffer = impl_.PopRxBuffer();
    if (!buffer) {
      return nullptr;
    }
    buffer->SetReturnLength(kReturnLength);
    return std::make_unique<RxFidlReturn>(std::move(buffer), kPort13);
  };

  libsync::Completion* rx_event = nullptr;
  SetEvtRxQueuePacketHandler(CreateTriggerRxHandler(&rx_event));

  auto wait_for_rx = [rx_event] {
    rx_event->Wait();
    rx_event->Reset();
  };

  auto assert_counters = [&port_connection](uint64_t frames, uint64_t bytes,
                                            std::string_view scope) {
    SCOPED_TRACE(scope);
    fidl::WireResult r = port_connection->GetCounters();
    ASSERT_OK(r.status());
    fidl::WireResponse rsp = std::move(r.value());
    ASSERT_TRUE(rsp.has_rx_bytes());
    ASSERT_EQ(rsp.rx_bytes(), bytes);
    ASSERT_TRUE(rsp.has_rx_frames());
    ASSERT_EQ(rsp.rx_frames(), frames);

    ASSERT_TRUE(rsp.has_tx_frames());
    EXPECT_EQ(rsp.tx_frames(), 0u);
    ASSERT_TRUE(rsp.has_tx_bytes());
    EXPECT_EQ(rsp.tx_bytes(), 0u);
  };
  // Counters should all be zero on creation.
  assert_counters(0, 0, "initial zeroes");

  // Return a single descriptor and assert the counters.
  {
    std::unique_ptr<RxFidlReturn> buffer = prepare_return_buffer();
    ASSERT_TRUE(buffer);
    RxFidlReturnTransaction txn(&impl_);
    txn.Enqueue(std::move(buffer));
    txn.Commit();
  }
  wait_for_rx();
  assert_counters(1, kReturnLength, "single buffer");

  // Return all the remaining descriptors and assert the counters.
  {
    RxFidlReturnTransaction txn(&impl_);
    for (size_t i = 1; i < std::size(kDescriptors); i++) {
      std::unique_ptr<RxFidlReturn> buffer = prepare_return_buffer();
      ASSERT_TRUE(buffer);
      txn.Enqueue(std::move(buffer));
    }
    txn.Commit();
  }
  wait_for_rx();
  assert_counters(std::size(kDescriptors), std::size(kDescriptors) * kReturnLength,
                  "remaining buffers");

  // There will be a session switch event when the session closes. Ensure that there is no longer a
  // callback that references a local variable.
  SetEvtRxQueuePacketHandler(nullptr);
}

TEST_F(NetworkDeviceTest, PortGetTxCounters) {
  ASSERT_OK(CreateDeviceWithPort13());

  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());

  zx::result port = OpenPort(kPort13);
  ASSERT_OK(port.status_value());
  fidl::WireSyncClient port_connection = std::move(port.value());

  constexpr uint32_t kTxLength = 17;
  const netdev::wire::PortId port_id = GetSaltedPortId(kPort13);

  constexpr uint16_t kDescriptors[] = {kDescriptorIndex0, kDescriptorIndex1, kDescriptorIndex2,
                                       kDescriptorIndex3};
  for (const uint16_t desc_idx : kDescriptors) {
    buffer_descriptor_t& desc = session.ResetDescriptor(desc_idx);
    desc.port_id.base = port_id.base;
    desc.port_id.salt = port_id.salt;
    desc.data_length = kTxLength;
  }

  auto assert_counters = [&port_connection](uint64_t frames, uint64_t bytes,
                                            std::string_view scope) {
    SCOPED_TRACE(scope);
    fidl::WireResult r = port_connection->GetCounters();
    ASSERT_OK(r.status());
    fidl::WireResponse rsp = std::move(r.value());
    ASSERT_TRUE(rsp.has_tx_bytes());
    EXPECT_EQ(rsp.tx_bytes(), bytes);
    ASSERT_TRUE(rsp.has_tx_frames());
    EXPECT_EQ(rsp.tx_frames(), frames);

    ASSERT_TRUE(rsp.has_rx_frames());
    EXPECT_EQ(rsp.rx_frames(), 0u);
    ASSERT_TRUE(rsp.has_rx_bytes());
    EXPECT_EQ(rsp.rx_bytes(), 0u);
  };
  // Counters should all be zero on creation.
  assert_counters(0, 0, "initial zeroes");

  // Send a single descriptor and assert the counters.
  {
    ASSERT_OK(session.SendTx(kDescriptors[0]));
    ASSERT_OK(WaitTx());
  }
  assert_counters(1, kTxLength, "single buffer");

  // Return all the remaining descriptors and assert the counters.
  {
    size_t actual = 0;
    ASSERT_OK(session.SendTx(std::begin(kDescriptors) + 1, std::size(kDescriptors) - 1, &actual));
    ASSERT_EQ(actual, std::size(kDescriptors) - 1);
    ASSERT_OK(WaitTx());
  }
  assert_counters(std::size(kDescriptors), std::size(kDescriptors) * kTxLength,
                  "remaining buffers");
}

TEST_F(NetworkDeviceTest, LogDebugInfoToSyslog) {
  ASSERT_OK(CreateDeviceWithPort13());
  bool bt_requested = false;
  SetBacktraceCallback([&bt_requested]() { bt_requested = true; });
  zx::result port = OpenPort(kPort13);
  ASSERT_OK(port.status_value());
  fidl::WireSyncClient port_connection = std::move(port.value());

  auto [client_end, server_end] = fidl::Endpoints<netdev::Diagnostics>::Create();
  ASSERT_OK(port_connection->GetDiagnostics(std::move(server_end)).status());

  fidl::WireSyncClient diagnostics{std::move(client_end)};
  ASSERT_OK(diagnostics->LogDebugInfoToSyslog().status());
  EXPECT_TRUE(bt_requested);
}

TEST_F(NetworkDeviceTest, TooManySessions) {
  ASSERT_OK(CreateDeviceWithPort13());

  std::array<TestSession, MAX_VMOS> sessions;
  for (TestSession& s : sessions) {
    ASSERT_OK(OpenSession(&s));
  }

  TestSession one_too_many;
  ASSERT_STATUS(OpenSession(&one_too_many), ZX_ERR_NO_RESOURCES);
}

// Subclass for stress tests with diminished logging to decrease noise.
class NetworkDeviceStressTest : public NetworkDeviceTest {
  void SetUp() override {
    NetworkDeviceTest::SetUp();
    fx_logger_config_t log_cfg = {
        .min_severity = FX_LOG_INFO,
        .tags = nullptr,
        .num_tags = 0,
    };
    fx_log_reconfigure(&log_cfg);
  }
};

// Guards against regression where tx queue would improperly install too many
// async waits, causing the process to be killed with TOO_MANY_OBSERVERS
// policy violation.
TEST_F(NetworkDeviceStressTest, ManyTxFullWaits) {
  impl_.set_immediate_return_tx(true);
  ASSERT_OK(CreateDeviceWithPort13());

  TestSession session;
  ASSERT_OK(OpenSession(&session));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitStart());

  std::array<TestSession, MAX_VMOS - 1> idle_sessions;

  for (TestSession& s : idle_sessions) {
    ASSERT_OK(OpenSession(&s));
    ASSERT_OK(AttachSessionPort(s, port13_));
  }

  const netdev::wire::PortId port13_id = GetSaltedPortId(kPort13);

  std::array<uint16_t, kDefaultTxDepth> descriptors;
  for (uint16_t i = 0; i < kDefaultTxDepth; i++) {
    buffer_descriptor_t& buffer = session.ResetDescriptor(i);
    buffer.port_id = {
        .base = port13_id.base,
        .salt = port13_id.salt,
    };
    descriptors[i] = i;
  }
  // Run for enough iterations to cause installations to explode. Policy
  // violation happens at a default 50000 observers. We have a number of idle
  // sessions to accelerate the observer install rate. The factor of 2 is for
  // extra protection around the default observer count which exists at a
  // distance.
  const size_t iterations = 2 * (50000 / idle_sessions.size() + 1);
  for (size_t i = 0; i < iterations; i++) {
    SCOPED_TRACE(i);
    size_t actual;
    ASSERT_OK(session.SendTx(descriptors.data(), descriptors.size(), &actual));
    ASSERT_EQ(actual, descriptors.size());
    ASSERT_OK(session.tx_fifo().wait_one(ZX_FIFO_READABLE, TEST_DEADLINE, nullptr));
    ASSERT_OK(session.FetchTx(descriptors.data(), descriptors.size(), &actual));
    ASSERT_EQ(actual, descriptors.size());
  }
}

// Test that QueueTx correctly splits up large numbers of buffers into batches
// that fit in the maximum FIDL message size.
TEST_F(NetworkDeviceTest, QueueTxBatches) {
  constexpr uint16_t kTxDescriptorCount = MAX_TX_BUFFERS + 1;
  impl_.info().tx_depth = kTxDescriptorCount;
  ASSERT_OK(CreateDeviceWithPort13());
  TestSession session;
  ASSERT_OK(OpenSession(&session, netdev::wire::SessionFlags::kPrimary,
                        kTxDescriptorCount + kDefaultRxDepth));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());

  // Send enough tx buffers that netdevice will have to queue them to the parent
  // driver in batches.
  std::array<uint16_t, kTxDescriptorCount> descriptors;
  std::iota(descriptors.begin(), descriptors.end(), 0);
  const netdev::wire::PortId port13_id = GetSaltedPortId(kPort13);
  for (uint16_t i = 0; i < kTxDescriptorCount; ++i) {
    buffer_descriptor_t& desc = session.ResetDescriptor(i);
    desc.port_id = {
        .base = port13_id.base,
        .salt = port13_id.salt,
    };
  }
  size_t actual;
  ASSERT_OK(session.SendTx(descriptors.data(), kTxDescriptorCount, &actual));
  ASSERT_EQ(actual, kTxDescriptorCount);

  // Wait for all the tx buffers to be queued.
  while (impl_.tx_buffer_count() < kTxDescriptorCount) {
    ASSERT_OK(WaitTx());
  }
  ASSERT_EQ(impl_.queue_tx_called(), MAX_TX_BUFFERS);
  ASSERT_EQ(impl_.queue_tx_called(), 1u);
}

// Test that QueueRxSpace correctly splits up large numbers of buffers into
// batches that fit in the maximum FIDL message size.
TEST_F(NetworkDeviceTest, QueueRxSpaceBatches) {
  constexpr uint16_t kRxDescriptorCount = MAX_RX_SPACE_BUFFERS + 1;
  impl_.info().rx_depth = kRxDescriptorCount;
  ASSERT_OK(CreateDeviceWithPort13());
  TestSession session;
  ASSERT_OK(OpenSession(&session, netdev::wire::SessionFlags::kPrimary,
                        kRxDescriptorCount + kDefaultTxDepth));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());

  // Send enough rx space buffers that netdevice will have to queue them to the
  // parent driver in batches.
  std::array<uint16_t, kRxDescriptorCount> descriptors;
  std::iota(descriptors.begin(), descriptors.end(), 0);
  for (uint16_t i = 0; i < kRxDescriptorCount; ++i) {
    session.ResetDescriptor(i);
  }
  size_t actual;
  ASSERT_OK(session.SendRx(descriptors.data(), kRxDescriptorCount, &actual));
  ASSERT_EQ(actual, kRxDescriptorCount);

  // Wait for all the rx space buffers to be queued.
  while (impl_.rx_buffer_count() < kRxDescriptorCount) {
    ASSERT_OK(WaitRxAvailable());
  }
  ASSERT_EQ(impl_.queue_rx_space_called(), MAX_RX_SPACE_BUFFERS);
  ASSERT_EQ(impl_.queue_rx_space_called(), 1u);
}

// Tests that leases are never given to sessions that don't have the lease
// delegation flag.
TEST_F(NetworkDeviceTest, SessionLeasesRequiresFlag) {
  fdf::Arena arena('NETD');
  ASSERT_OK(CreateDeviceWithPort13());

  TestSession session;
  ASSERT_OK(OpenSession(&session, netdev::wire::SessionFlags::kPrimary));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());
  // A lease with frame 0 should always be fulfilled, but we're _not_ going to
  // observe it in a session without the flag. Instead the lease is dropped
  // internally.
  auto [lease, chan] = CreateDelegatedLease(0);
  fidl::OneWayStatus status =
      impl_.client().buffer(arena)->DelegateRxLease(fidl::ToWire(arena, std::move(lease)));
  ASSERT_OK(status.status());
  zx_signals_t observed;
  ASSERT_OK(chan.wait_one(ZX_CHANNEL_PEER_CLOSED, TEST_DEADLINE, &observed));
  ASSERT_EQ(observed, ZX_CHANNEL_PEER_CLOSED);
}

// Tests that calling watch when a lease is ready to be consumed returns
// immediately.
TEST_F(NetworkDeviceTest, SessionLeaseImmediateReturn) {
  fdf::Arena arena('NETD');
  ASSERT_OK(CreateDeviceWithPort13());

  TestSession session;
  ASSERT_OK(OpenSession(&session, netdev::wire::SessionFlags::kPrimary |
                                      netdev::wire::SessionFlags::kReceiveRxPowerLeases));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());
  // A lease with frame 0 should always be fulfilled.
  auto [lease, chan] = CreateDelegatedLease(0);
  fidl::OneWayStatus status =
      impl_.client().buffer(arena)->DelegateRxLease(fidl::ToWire(arena, std::move(lease)));
  ASSERT_OK(status.status());
  fidl::WireResult result = session.session()->WatchDelegatedRxLease();
  ASSERT_OK(result.status());
  netdev::wire::DelegatedRxLease& received_lease = result.value().lease;
  ASSERT_TRUE(received_lease.has_hold_until_frame());
  EXPECT_EQ(received_lease.hold_until_frame(), 0u);
  ASSERT_TRUE(received_lease.has_handle());
  ASSERT_EQ(received_lease.handle().Which(), netdev::wire::DelegatedRxLeaseHandle::Tag::kChannel);
  EXPECT_EQ(fsl::GetKoid(received_lease.handle().channel().get()), fsl::GetRelatedKoid(chan.get()));
}

// Tests that leases are never given to sessions that don't have the lease
// delegation flag.
TEST_F(NetworkDeviceTest, SessionLeaseDelegation) {
  ASSERT_OK(CreateDeviceWithPort13());
  static constexpr uint8_t kPort5 = 5;
  FakeNetworkPortImpl port5;
  ASSERT_OK(port5.AddPort(kPort5, impl_dispatcher_.get(), OpenConnection(), impl_.client()));
  auto cleanup = fit::defer([&port5]() { port5.RemoveSync(); });

  TestSession session;
  ASSERT_OK(OpenSession(&session, netdev::wire::SessionFlags::kPrimary |
                                      netdev::wire::SessionFlags::kReceiveRxPowerLeases));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());
  constexpr uint16_t kBufferCount = 3;
  uint16_t desc_buff[kBufferCount];
  for (uint16_t i = 0; i < kBufferCount; i++) {
    session.ResetDescriptor(i);
    desc_buff[i] = i;
  }
  ASSERT_OK(session.SendRx(desc_buff, kBufferCount, nullptr));
  ASSERT_OK(WaitRxAvailable());

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  fidl::WireClient<netdev::Session> session_cli(session.session().TakeClientEnd(),
                                                loop.dispatcher());
  std::optional<zx::result<netdev::DelegatedRxLease>> watch_result;
  session_cli->WatchDelegatedRxLease().Then([&watch_result](auto& result) {
    if (!result.ok()) {
      watch_result = zx::error_result(result.status());
      return;
    }
    watch_result = fit::ok(fidl::ToNatural(result->lease));
  });

  ASSERT_OK(loop.RunUntilIdle());
  ASSERT_FALSE(watch_result.has_value());

  // Create a lease that will fulfill on the second frame.
  fdf::Arena arena('NETD');
  auto [lease, chan] = CreateDelegatedLease(2);
  fidl::OneWayStatus status =
      impl_.client().buffer(arena)->DelegateRxLease(fidl::ToWire(arena, std::move(lease)));
  ASSERT_OK(status.status());

  constexpr uint16_t kDataLen = 10;
  RxFidlReturnTransaction return_session(&impl_);
  // Send 3 buffers, only one of them should make it to the session.
  // One buffer with zero bytes written.
  {
    std::unique_ptr buff = impl_.PopRxBuffer();
    ASSERT_TRUE(buff);
    return_session.Enqueue(std::move(buff), kPort13);
  }
  // One buffer with bytes written.
  {
    std::unique_ptr buff = impl_.PopRxBuffer();
    ASSERT_TRUE(buff);
    std::vector<uint8_t> data(kDataLen, static_cast<uint8_t>(0xAA));
    ASSERT_OK(buff->WriteData(data, impl_.VmoGetter()));
    return_session.Enqueue(std::move(buff), kPort13);
  }
  // Another buffer with bytes written but to a port the session is not
  // interested in.
  {
    std::unique_ptr buff = impl_.PopRxBuffer();
    ASSERT_TRUE(buff);
    std::vector<uint8_t> data(kDataLen, static_cast<uint8_t>(0xAA));
    ASSERT_OK(buff->WriteData(data, impl_.VmoGetter()));
    return_session.Enqueue(std::move(buff), kPort5);
  }

  return_session.Commit();

  ASSERT_OK(loop.Run(TEST_DEADLINE, true));
  ASSERT_TRUE(watch_result.has_value());
  ASSERT_OK(watch_result.value().status_value());
  netdev::DelegatedRxLease& received_lease = watch_result.value().value();
  ASSERT_TRUE(received_lease.hold_until_frame().has_value());
  // Session observes a single frame so this is what the delegated lease should
  // show.
  EXPECT_EQ(received_lease.hold_until_frame().value(), 1u);
  ASSERT_TRUE(received_lease.handle().has_value());
  ASSERT_EQ(received_lease.handle().value().Which(), netdev::DelegatedRxLeaseHandle::Tag::kChannel);
  EXPECT_EQ(fsl::GetKoid(chan.get()),
            fsl::GetRelatedKoid(received_lease.handle().value().channel().value().get()));
}

// Tests that the session channel is closed if two pending rx lease watches are
// created.
TEST_F(NetworkDeviceTest, SessionNoDualLeaseWatch) {
  ASSERT_OK(CreateDeviceWithPort13());
  TestSession session;
  ASSERT_OK(OpenSession(&session, netdev::wire::SessionFlags::kPrimary));
  ASSERT_OK(AttachSessionPort(session, port13_));
  ASSERT_OK(WaitSessionStarted());
  ASSERT_OK(WaitStart());

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  fidl::WireClient<netdev::Session> session_cli(session.session().TakeClientEnd(),
                                                loop.dispatcher());
  std::array<std::optional<zx::result<netdev::DelegatedRxLease>>, 2> results;
  for (auto& r : results) {
    session_cli->WatchDelegatedRxLease().Then([r = &r](auto& result) {
      if (!result.ok()) {
        *r = zx::error_result(result.status());
        return;
      }
      *r = fit::ok(fidl::ToNatural(result->lease));
    });
  }

  ASSERT_OK(loop.Run(TEST_DEADLINE, true));

  for (size_t i = 0; i < results.size(); i++) {
    SCOPED_TRACE(i);
    auto& r = results[i];
    ASSERT_TRUE(r.has_value());
    // We should observe the epitaph error on both results.
    EXPECT_EQ(r.value().status_value(), ZX_ERR_BAD_STATE);
  }
}

class NetworkDeviceShimTest : public ::testing::Test {
 public:
  NetworkDeviceShimTest() = default;
  void SetUp() override {
    auto ifc_dispatcher = fdf::SynchronizedDispatcher::Create(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "shim-test-ifc",
        [this](fdf_dispatcher_t*) { ifc_dispatcher_shutdown_.Signal(); });
    ASSERT_OK(ifc_dispatcher.status_value());
    ifc_dispatcher_ = std::move(ifc_dispatcher.value());

    zx::result shim_dispatchers = OwnedShimDispatchers::Create();
    ASSERT_OK(shim_dispatchers.status_value());
    shim_dispatchers_ = std::move(shim_dispatchers.value());

    CreateShim();
  }

  void TearDown() override {
    if (ifc_dispatcher_.get()) {
      ifc_dispatcher_.ShutdownAsync();
      ifc_dispatcher_shutdown_.Wait();
    }
    if (shim_dispatchers_) {
      shim_dispatchers_->ShutdownSync();
    }

    if (shim_) {
      // With the dispatchers shut down this should be synchronous.
      ASSERT_EQ(shim_->Teardown([] {}), NetworkDeviceImplBinder::Synchronicity::Sync);
    }
  }

  zx_status_t InitImpl() {
    auto ifc_client_end = ifc_.Bind(&ifc_dispatcher_);
    if (ifc_client_end.is_error()) {
      return ifc_client_end.status_value();
    }

    fdf::Arena arena('NETD');
    auto result = fidl_impl_.buffer(arena)->Init(std::move(ifc_client_end.value()));
    if (!result.ok()) {
      return result.status();
    }
    return result->s;
  }

  zx_status_t AddPortSync(uint8_t port_id, network_port_protocol_t* proto) {
    using Context = std::pair<libsync::Completion, zx_status_t>;
    Context context;

    shim_->NetworkDeviceIfcAddPort(
        port_id, proto,
        [](void* ctx, zx_status_t status) {
          Context* context = static_cast<Context*>(ctx);
          context->second = status;
          context->first.Signal();
        },
        &context);
    context.first.Wait();
    return context.second;
  }

 protected:
  fdf_testing::DriverRuntime driver_runtime_;
  // This is the client that the shim will call into, i.e. the vendor driver.
  banjo::FakeNetworkDeviceImpl banjo_impl_;
  // This is the client that the shim will serve, i.e. the netdevice driver.
  fdf::WireSyncClient<netdriver::NetworkDeviceImpl> fidl_impl_;
  FakeNetworkDeviceIfc ifc_;
  std::unique_ptr<NetworkDeviceShim> shim_;

  fdf::Dispatcher ifc_dispatcher_;
  libsync::Completion ifc_dispatcher_shutdown_;
  std::unique_ptr<OwnedShimDispatchers> shim_dispatchers_;

 private:
  void CreateShim() {
    auto proto = banjo_impl_.proto();
    shim_ = std::make_unique<NetworkDeviceShim>(&proto, shim_dispatchers_->Unowned());
    auto fidl_impl = shim_->Bind();
    ASSERT_OK(fidl_impl.status_value());
    fidl_impl_.Bind(std::move(fidl_impl.value()));
  }
};

TEST_F(NetworkDeviceShimTest, BindAndTeardown) {
  // Verify that a bound NetworkDeviceShim can be safely destroyed in its teardown callback.

  libsync::Completion teardown_complete;
  NetworkDeviceImplBinder::Synchronicity synchronicity = shim_->Teardown([&] {
    shim_.reset();
    teardown_complete.Signal();
  });
  ASSERT_EQ(synchronicity, NetworkDeviceImplBinder::Synchronicity::Async);
  teardown_complete.Wait();
}

TEST_F(NetworkDeviceShimTest, TeardownFromOtherDispatcher) {
  // Verify that a bound NetworkDeviceShim can be safely torn down from a dispatcher with a
  // different driver owner. This is a tricky case because it allows posted tasks to be inlined
  // which can cause unbinding to deadlock. The different driver owner case happens because the shim
  // dispatchers are created with a different owner to support inlining.

  libsync::Completion teardown_complete;
  // The ifc dispatcher has a different owner, post a task to it to set up the correct prerequisites
  // for inlining.
  async::PostTask(ifc_dispatcher_.async_dispatcher(),
                  [&]() { shim_->Teardown([&] { teardown_complete.Signal(); }); });
  teardown_complete.Wait();
}

TEST_F(NetworkDeviceShimTest, AddPort) {
  // Verify that AddPort works and manages the lifetime of the NetworkPortShim object correctly.
  ASSERT_OK(InitImpl());

  constexpr uint8_t kPortId = 13;
  ifc_.add_port_ = [&](netdriver::wire::NetworkDeviceIfcAddPortRequest* request, fdf::Arena& arena,
                       FakeNetworkDeviceIfc::AddPortCompleter::Sync& completer) {
    ASSERT_EQ(request->id, kPortId);
    completer.buffer(arena).Reply(ZX_OK);
  };

  network_port_protocol_t port_proto{};
  ASSERT_OK(AddPortSync(kPortId, &port_proto));
}

TEST_F(NetworkDeviceShimTest, GetMac) {
  // Verify that GetMac works and manages the lifetime of the MacAddrShim object correctly.
  ASSERT_OK(InitImpl());

  fdf::WireSyncClient<netdriver::NetworkPort> port_client;
  ifc_.add_port_ = [&](netdriver::wire::NetworkDeviceIfcAddPortRequest* request, fdf::Arena& arena,
                       FakeNetworkDeviceIfc::AddPortCompleter::Sync& completer) {
    port_client.Bind(std::move(request->port));
    completer.buffer(arena).Reply(ZX_OK);
  };

  banjo::FakeNetworkPortImpl port;
  auto port_proto = port.protocol();
  ASSERT_OK(AddPortSync(12, &port_proto));

  fdf::Arena arena('NETD');
  auto mac = port_client.buffer(arena)->GetMac();
  ASSERT_OK(mac.status());
}

TEST_F(NetworkDeviceShimTest, TeardownAndThenPortClientUnbinds) {
  // Verify asynchronous Teardown when a port client is still bound during the teardown call and
  // later unbound.
  ASSERT_OK(InitImpl());

  fdf::WireSyncClient<netdriver::NetworkPort> port_client;
  ifc_.add_port_ = [&](netdriver::wire::NetworkDeviceIfcAddPortRequest* request, fdf::Arena& arena,
                       FakeNetworkDeviceIfc::AddPortCompleter::Sync& completer) {
    port_client.Bind(std::move(request->port));
    completer.buffer(arena).Reply(ZX_OK);
  };

  banjo::FakeNetworkPortImpl port;
  auto port_proto = port.protocol();
  ASSERT_OK(AddPortSync(12, &port_proto));

  fdf::Arena arena('NETD');
  auto mac = port_client.buffer(arena)->GetMac();
  ASSERT_OK(mac.status());

  libsync::Completion teardown_complete;
  ASSERT_EQ(shim_->Teardown([&] { teardown_complete.Signal(); }),
            NetworkDeviceImplBinder::Synchronicity::Async);
  ASSERT_FALSE(teardown_complete.signaled());
  // Unbinding the client (by destroying it) should trigger port removal and complete the teardown.
  port_client = {};
  teardown_complete.Wait();
}

}  // namespace testing
}  // namespace network
