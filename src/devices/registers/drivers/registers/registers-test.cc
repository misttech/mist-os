// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "registers.h"

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/testing/cpp/driver_runtime.h>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <mock-mmio-reg/mock-mmio-reg.h>

#include "src/devices/lib/fidl-metadata/registers.h"

namespace registers {

namespace {

constexpr size_t kRegSize = 0x00000100;

}  // namespace

template <typename T>
class FakeRegistersDevice : public RegistersDevice<T> {
 public:
  explicit FakeRegistersDevice()
      : RegistersDevice<T>(nullptr), checker_(fdf::Dispatcher::GetCurrent()->async_dispatcher()) {}

  void AddRegister(RegistersMetadataEntry config) {
    std::lock_guard guard(checker_);
    registers_.push_back(
        std::make_unique<Register<T>>(nullptr, RegistersDevice<T>::mmios_[config.mmio_id()]));
    registers_.back()->Init(config);

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_registers::Device>();
    if (endpoints.is_error()) {
      return;
    }

    zx::result outgoing_client = registers_.back()->CreateAndServeOutgoingDirectory();
    if (outgoing_client.is_error()) {
      return;
    }

    zx::result connect_result = component::ConnectAt<fuchsia_hardware_registers::Device>(
        outgoing_client.value(), std::move(endpoints->server),
        component::kServiceDirectoryTrailingSlash +
            component::MakeServiceMemberPath<fuchsia_hardware_registers::Service::Device>(
                component::kDefaultInstance));
    if (connect_result.is_error()) {
      return;
    }

    clients_.emplace(config.bind_id(),
                     std::make_shared<fidl::WireSyncClient<fuchsia_hardware_registers::Device>>(
                         std::move(endpoints->client)));
  }

  std::shared_ptr<fidl::WireSyncClient<fuchsia_hardware_registers::Device>> GetClient(uint64_t id) {
    std::lock_guard guard(checker_);
    return clients_[id];
  }

  zx_status_t Init(std::map<uint32_t, std::shared_ptr<MmioInfo>> mmios) {
    std::lock_guard guard(checker_);
    return RegistersDevice<T>::Init(std::move(mmios));
  }

 private:
  async::synchronization_checker checker_;
  std::vector<std::unique_ptr<Register<T>>> registers_ __TA_GUARDED(checker_);
  std::map<uint64_t, std::shared_ptr<fidl::WireSyncClient<fuchsia_hardware_registers::Device>>>
      clients_ __TA_GUARDED(checker_);
};

// Wraps a FakeRegistersDevice inside a |TestDispatcherBound| and provides pass-through helpers
// for calling |SyncCall| on the wrapped |FakeRegistersDevice|.
template <typename T>
class FakeRegistersDeviceWrapper {
 public:
  explicit FakeRegistersDeviceWrapper(async_dispatcher_t* dispatcher)
      : registers_(dispatcher, std::in_place) {}

  void AddRegister(RegistersMetadataEntry config) {
    registers_.SyncCall(&FakeRegistersDevice<T>::AddRegister, config);
  }

  std::shared_ptr<fidl::WireSyncClient<fuchsia_hardware_registers::Device>> GetClient(uint64_t id) {
    return registers_.SyncCall(&FakeRegistersDevice<T>::GetClient, id);
  }

  zx_status_t Init(std::map<uint32_t, std::shared_ptr<MmioInfo>> mmios) {
    return registers_.SyncCall(&FakeRegistersDevice<T>::Init, std::move(mmios));
  }

 private:
  async_patterns::TestDispatcherBound<FakeRegistersDevice<T>> registers_;
};

class RegistersDeviceTest : public zxtest::Test {
 public:
  template <typename T>
  std::unique_ptr<FakeRegistersDeviceWrapper<T>> Init(uint32_t mmio_count) {
    fbl::AllocChecker ac;

    std::map<uint32_t, std::shared_ptr<MmioInfo>> mmios;
    for (uint32_t i = 0; i < mmio_count; i++) {
      mock_mmio_.push_back(
          std::make_unique<ddk_mock::MockMmioRegRegion>(sizeof(T), kRegSize / sizeof(T)));

      std::vector<fbl::Mutex> locks(kRegSize / sizeof(T));
      mmios.emplace(i, std::make_shared<MmioInfo>(MmioInfo{
                           .mmio = mock_mmio_[i]->GetMmioBuffer(),
                           .locks = std::move(locks),
                       }));
    }

    auto device =
        std::make_unique<FakeRegistersDeviceWrapper<T>>(registers_dispatcher_->async_dispatcher());
    device->Init(std::move(mmios));
    return device;
  }

  void TearDown() override {
    for (const auto& i : mock_mmio_) {
      ASSERT_NO_FATAL_FAILURE(i->VerifyAll());
    }
  }

 protected:
  // The |Register| device lives on a driver runtime background dispatcher since it uses the
  // |fdf::Dispatcher::GetCurrent()| API. It serves the FIDL protocol on this dispatcher.
  // This dispatcher must be separate from the test as the test calls synchronously into
  // this FIDL protocol.
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher registers_dispatcher_ = runtime_.StartBackgroundDispatcher();

  std::vector<std::unique_ptr<ddk_mock::MockMmioRegRegion>> mock_mmio_;
  fidl::Arena<2048> allocator_;
};

TEST_F(RegistersDeviceTest, Read32Test) {
  auto device = Init<uint32_t>(/* mmio_count: */ 3);
  ASSERT_NOT_NULL(device);

  device->AddRegister(fuchsia_hardware_registers::wire::RegistersMetadataEntry::Builder(allocator_)
                          .bind_id(0)
                          .mmio_id(0)
                          .masks(std::vector<fuchsia_hardware_registers::wire::MaskEntry>{
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR32(0xFFFFFFFF))
                                  .mmio_offset(0x0)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build()})
                          .Build());
  device->AddRegister(fuchsia_hardware_registers::wire::RegistersMetadataEntry::Builder(allocator_)
                          .bind_id(1)
                          .mmio_id(2)
                          .masks(std::vector<fuchsia_hardware_registers::wire::MaskEntry>{
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR32(0xFFFFFFFF))
                                  .mmio_offset(0x0)
                                  .count(2)
                                  .overlap_check_on(true)
                                  .Build(),
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR32(0xFFFF0000))
                                  .mmio_offset(0x8)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build(),
                          })
                          .Build());

  // Invalid Call
  auto invalid_call_result =
      (*device->GetClient(0))->ReadRegister8(/* offset: */ 0x0, /* mask: */ 0xFF);
  ASSERT_TRUE(invalid_call_result.ok());
  EXPECT_FALSE(invalid_call_result->is_ok());

  // Address not aligned
  auto unaligned_result =
      (*device->GetClient(0))->ReadRegister32(/* offset: */ 0x1, /* mask: */ 0xFFFFFFFF);
  EXPECT_TRUE(unaligned_result.ok());
  EXPECT_FALSE(unaligned_result->is_ok());

  // Address out of range
  auto out_of_range_result =
      (*device->GetClient(1))->ReadRegister32(/* offset: */ 0xC, /* mask: */ 0xFFFFFFFF);
  ASSERT_TRUE(out_of_range_result.ok());
  EXPECT_FALSE(out_of_range_result->is_ok());

  // Invalid mask
  auto invalid_mask_result =
      (*device->GetClient(1))->ReadRegister32(/* offset: */ 0x8, /* mask: */ 0xFFFFFFFF);
  EXPECT_TRUE(invalid_mask_result.ok());
  EXPECT_FALSE(invalid_mask_result->is_ok());

  // Successful
  (*(mock_mmio_[0]))[0x0].ExpectRead(0x12341234);
  auto read_result1 =
      (*device->GetClient(0))->ReadRegister32(/* offset: */ 0x0, /* mask: */ 0xFFFFFFFF);
  ASSERT_TRUE(read_result1.ok());
  ASSERT_TRUE(read_result1->is_ok());
  EXPECT_EQ(read_result1->value()->value, 0x12341234);

  (*(mock_mmio_[2]))[0x4].ExpectRead(0x12341234);
  auto read_result2 =
      (*device->GetClient(1))->ReadRegister32(/* offset: */ 0x4, /* mask: */ 0xFFFF0000);
  EXPECT_TRUE(read_result2.ok());
  EXPECT_TRUE(read_result2->is_ok());
  EXPECT_EQ(read_result2->value()->value, 0x12340000);

  (*(mock_mmio_[2]))[0x8].ExpectRead(0x12341234);
  auto read_result3 =
      (*device->GetClient(1))->ReadRegister32(/* offset: */ 0x8, /* mask: */ 0xFFFF0000);
  EXPECT_TRUE(read_result3.ok());
  EXPECT_TRUE(read_result3->is_ok());
  EXPECT_EQ(read_result3->value()->value, 0x12340000);
}

TEST_F(RegistersDeviceTest, Write32Test) {
  auto device = Init<uint32_t>(/* mmio_count: */ 2);
  ASSERT_NOT_NULL(device);

  device->AddRegister(fuchsia_hardware_registers::wire::RegistersMetadataEntry::Builder(allocator_)
                          .bind_id(0)
                          .mmio_id(0)
                          .masks(std::vector<fuchsia_hardware_registers::wire::MaskEntry>{
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR32(0xFFFFFFFF))
                                  .mmio_offset(0x0)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build()})
                          .Build());
  device->AddRegister(fuchsia_hardware_registers::wire::RegistersMetadataEntry::Builder(allocator_)
                          .bind_id(1)
                          .mmio_id(1)
                          .masks(std::vector<fuchsia_hardware_registers::wire::MaskEntry>{
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR32(0xFFFFFFFF))
                                  .mmio_offset(0x0)
                                  .count(2)
                                  .overlap_check_on(true)
                                  .Build(),
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR32(0xFFFF0000))
                                  .mmio_offset(0x8)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build(),
                          })
                          .Build());

  // Invalid Call
  auto invalid_call_result =
      (*device->GetClient(0))
          ->WriteRegister8(/* offset: */ 0x0, /* mask: */ 0xFF, /* value:  */ 0x12);
  ASSERT_TRUE(invalid_call_result.ok());
  EXPECT_FALSE(invalid_call_result->is_ok());

  // Address not aligned
  auto unaligned_result =
      (*device->GetClient(0))
          ->WriteRegister32(
              /* offset: */ 0x1, /* mask: */ 0xFFFFFFFF, /* value: */ 0x43214321);
  ASSERT_TRUE(unaligned_result.ok());
  EXPECT_FALSE(unaligned_result->is_ok());

  // Address out of range
  auto out_of_range_result =
      (*device->GetClient(1))
          ->WriteRegister32(
              /* offset: */ 0xC, /* mask: */ 0xFFFFFFFF, /* value: */ 0x43214321);
  EXPECT_TRUE(out_of_range_result.ok());
  EXPECT_FALSE(out_of_range_result->is_ok());

  // Invalid mask
  auto invalid_mask_result =
      (*device->GetClient(1))
          ->WriteRegister32(
              /* offset: */ 0x8, /* mask: */ 0xFFFFFFFF, /* value: */ 0x43214321);
  EXPECT_TRUE(invalid_mask_result.ok());
  EXPECT_FALSE(invalid_mask_result->is_ok());

  // Successful
  (*(mock_mmio_[0]))[0x0].ExpectRead(0x00000000).ExpectWrite(0x43214321);
  auto read_result1 = (*device->GetClient(0))
                          ->WriteRegister32(
                              /* offset: */ 0x0, /* mask: */ 0xFFFFFFFF, /* value: */ 0x43214321);
  EXPECT_TRUE(read_result1.ok());
  EXPECT_TRUE(read_result1->is_ok());

  (*(mock_mmio_[1]))[0x4].ExpectRead(0x00000000).ExpectWrite(0x43210000);
  auto read_result2 = (*device->GetClient(1))
                          ->WriteRegister32(
                              /* offset: */ 0x4, /* mask: */ 0xFFFF0000, /* value: */ 0x43214321);
  EXPECT_TRUE(read_result2.ok());
  EXPECT_TRUE(read_result2->is_ok());

  (*(mock_mmio_[1]))[0x8].ExpectRead(0x00000000).ExpectWrite(0x43210000);
  auto read_result3 = (*device->GetClient(1))
                          ->WriteRegister32(
                              /* offset: */ 0x8, /* mask: */ 0xFFFF0000, /* value: */ 0x43214321);
  EXPECT_TRUE(read_result3.ok());
  EXPECT_TRUE(read_result3->is_ok());
}

TEST_F(RegistersDeviceTest, Read64Test) {
  auto device = Init<uint64_t>(/* mmio_count: */ 3);
  ASSERT_NOT_NULL(device);

  device->AddRegister(fuchsia_hardware_registers::wire::RegistersMetadataEntry::Builder(allocator_)
                          .bind_id(0)
                          .mmio_id(0)
                          .masks(std::vector<fuchsia_hardware_registers::wire::MaskEntry>{
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR64(
                                      allocator_, 0xFFFFFFFFFFFFFFFF))
                                  .mmio_offset(0x0)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build()})
                          .Build());
  device->AddRegister(fuchsia_hardware_registers::wire::RegistersMetadataEntry::Builder(allocator_)
                          .bind_id(1)
                          .mmio_id(2)
                          .masks(std::vector<fuchsia_hardware_registers::wire::MaskEntry>{
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR64(
                                      allocator_, 0xFFFFFFFFFFFFFFFF))
                                  .mmio_offset(0x0)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build(),
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR64(
                                      allocator_, 0x00000000FFFFFFFF))
                                  .mmio_offset(0x8)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build(),
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR64(
                                      allocator_, 0x0000FFFFFFFF0000))
                                  .mmio_offset(0x10)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build(),
                          })
                          .Build());

  // Invalid Call
  auto invalid_call_result =
      (*device->GetClient(0))->ReadRegister8(/* offset: */ 0x0, /* mask: */ 0xFF);
  ASSERT_TRUE(invalid_call_result.ok());
  EXPECT_FALSE(invalid_call_result->is_ok());

  // Address not aligned
  auto unaligned_result =
      (*device->GetClient(0))->ReadRegister64(/* offset: */ 0x1, /* mask: */ 0xFFFFFFFFFFFFFFFF);
  ASSERT_TRUE(unaligned_result.ok());
  EXPECT_FALSE(unaligned_result->is_ok());

  // Address out of range
  auto out_of_range_result =
      (*device->GetClient(1))->ReadRegister64(/* offset: */ 0x20, /* mask: */ 0xFFFFFFFFFFFFFFFF);
  ASSERT_TRUE(out_of_range_result.ok());
  EXPECT_FALSE(out_of_range_result->is_ok());

  // Invalid mask
  auto invalid_mask_result =
      (*device->GetClient(1))->ReadRegister64(/* offset: */ 0x8, /* mask: */ 0xFFFFFFFFFFFFFFFF);
  ASSERT_TRUE(invalid_mask_result.ok());
  EXPECT_FALSE(invalid_mask_result->is_ok());

  // Successful
  (*(mock_mmio_[0]))[0x0].ExpectRead(0x1234123412341234);
  auto read_result1 =
      (*device->GetClient(0))->ReadRegister64(/* offset: */ 0x0, /* mask: */ 0xFFFFFFFFFFFFFFFF);
  ASSERT_TRUE(read_result1.ok());
  ASSERT_TRUE(read_result1->is_ok());
  EXPECT_EQ(read_result1->value()->value, 0x1234123412341234);

  (*(mock_mmio_[2]))[0x8].ExpectRead(0x1234123412341234);
  auto read_result2 =
      (*device->GetClient(1))->ReadRegister64(/* offset: */ 0x8, /* mask: */ 0x00000000FFFF0000);
  ASSERT_TRUE(read_result2.ok());
  ASSERT_TRUE(read_result2->is_ok());
  EXPECT_EQ(read_result2->value()->value, 0x0000000012340000);
}

TEST_F(RegistersDeviceTest, Write64Test) {
  auto device = Init<uint64_t>(/* mmio_count: */ 2);
  ASSERT_NOT_NULL(device);

  device->AddRegister(fuchsia_hardware_registers::wire::RegistersMetadataEntry::Builder(allocator_)
                          .bind_id(0)
                          .mmio_id(0)
                          .masks(std::vector<fuchsia_hardware_registers::wire::MaskEntry>{
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR64(
                                      allocator_, 0xFFFFFFFFFFFFFFFF))
                                  .mmio_offset(0x0)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build()})
                          .Build());
  device->AddRegister(fuchsia_hardware_registers::wire::RegistersMetadataEntry::Builder(allocator_)
                          .bind_id(1)
                          .mmio_id(1)
                          .masks(std::vector<fuchsia_hardware_registers::wire::MaskEntry>{
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR64(
                                      allocator_, 0xFFFFFFFFFFFFFFFF))
                                  .mmio_offset(0x0)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build(),
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR64(
                                      allocator_, 0x00000000FFFFFFFF))
                                  .mmio_offset(0x8)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build(),
                              fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator_)
                                  .mask(fuchsia_hardware_registers::wire::Mask::WithR64(
                                      allocator_, 0x0000FFFFFFFF0000))
                                  .mmio_offset(0x10)
                                  .count(1)
                                  .overlap_check_on(true)
                                  .Build(),
                          })
                          .Build());

  // Invalid Call
  auto invalid_call_result =
      (*device->GetClient(0))
          ->WriteRegister8(/* offset: */ 0x0, /* mask: */ 0xFF, /* value:  */ 0x12);
  ASSERT_TRUE(invalid_call_result.ok());
  EXPECT_FALSE(invalid_call_result->is_ok());

  // Address not aligned
  auto unaligned_result =
      (*device->GetClient(0))
          ->WriteRegister64(
              /* offset: */ 0x1, /* mask: */ 0xFFFFFFFFFFFFFFFF, /* value: */ 0x4321432143214321);
  ASSERT_TRUE(unaligned_result.ok());
  EXPECT_FALSE(unaligned_result->is_ok());

  // Address out of range
  auto out_of_range_result =
      (*device->GetClient(1))
          ->WriteRegister64(
              /* offset: */ 0x20, /* mask: */ 0xFFFFFFFFFFFFFFFF, /* value: */ 0x4321432143214321);
  ASSERT_TRUE(out_of_range_result.ok());
  EXPECT_FALSE(out_of_range_result->is_ok());

  // Invalid mask
  auto invalid_mask_result = (*device->GetClient(1))
                                 ->WriteRegister64(/* offset: */ 0x8,
                                                   /* mask: */ 0xFFFFFFFFFFFFFFFF,
                                                   /* value: */ 0x4321432143214321);
  ASSERT_TRUE(invalid_mask_result.ok());
  EXPECT_FALSE(invalid_mask_result->is_ok());

  // Successful
  (*(mock_mmio_[0]))[0x0].ExpectRead(0x0000000000000000).ExpectWrite(0x4321432143214321);
  auto read_result1 = (*device->GetClient(0))
                          ->WriteRegister64(
                              /* offset: */ 0x0, /* mask: */ 0xFFFFFFFFFFFFFFFF, /* value: */
                              0x4321432143214321);
  ASSERT_TRUE(read_result1.ok());
  EXPECT_TRUE(read_result1->is_ok());

  (*(mock_mmio_[1]))[0x8].ExpectRead(0x0000000000000000).ExpectWrite(0x0000000043210000);
  auto read_result2 = (*device->GetClient(1))
                          ->WriteRegister64(
                              /* offset: */ 0x8, /* mask: */ 0x00000000FFFF0000, /* value: */
                              0x0000000043210000);
  ASSERT_TRUE(read_result2.ok());
  EXPECT_TRUE(read_result2->is_ok());
}

}  // namespace registers
