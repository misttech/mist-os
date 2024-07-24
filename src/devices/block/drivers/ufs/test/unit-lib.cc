// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "unit-lib.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/fake-bti/bti.h>

#include <memory>

namespace ufs {

ufs_mock_device::UfsMockDevice *TestUfs::mock_device_;

void UfsTest::InitMockDevice() {
  std::unique_ptr<zx::interrupt> irq = std::make_unique<zx::interrupt>();
  ZX_ASSERT(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, irq.get()) == ZX_OK);

  // Configure mock device.
  mock_device_.Init(std::move(irq));
  ZX_ASSERT(mock_device_.AddLun(0) == ZX_OK);
}

void UfsTest::StartDriver() {
  // Initialize driver test environment.
  fuchsia_driver_framework::DriverStartArgs start_args;
  fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client;
  incoming_.SyncCall([&](IncomingNamespace *incoming) mutable {
    auto start_args_result = incoming->node.CreateStartArgsAndServe();
    ASSERT_TRUE(start_args_result.is_ok());
    start_args = std::move(start_args_result->start_args);
    outgoing_directory_client = std::move(start_args_result->outgoing_directory_client);

    ASSERT_OK(incoming->env.Initialize(std::move(start_args_result->incoming_directory_server)));
  });

  // Serve (fake) pci_server.
  incoming_.SyncCall([&](IncomingNamespace *incoming) mutable {
    {
      auto result = incoming->env.incoming_directory().AddService<fuchsia_hardware_pci::Service>(
          std::move(incoming->pci_server.GetInstanceHandler()), "pci");
      ASSERT_TRUE(result.is_ok());
    }
    incoming->pci_server.SetMockDevice(&mock_device_);
  });

  // Start dut_.
  TestUfs::SetMockDevice(&mock_device_);
  ASSERT_OK(runtime_.RunToCompletion(dut_.Start(std::move(start_args))));
}

void UfsTest::SetUp() {
  InitMockDevice();
  StartDriver();
}

void UfsTest::TearDown() {
  zx::result prepare_stop_result = runtime_.RunToCompletion(dut_.PrepareStop());
  EXPECT_OK(prepare_stop_result.status_value());

  incoming_.reset();
  runtime_.ShutdownAllDispatchers(fdf::Dispatcher::GetCurrent()->get());

  EXPECT_OK(dut_.Stop());
}

zx_status_t UfsTest::DisableController() { return dut_->DisableHostController(); }

zx_status_t UfsTest::EnableController() { return dut_->EnableHostController(); }

zx::result<> UfsTest::FillDescriptorAndSendRequest(uint8_t slot, DataDirection ddir,
                                                   uint16_t resp_offset, uint16_t resp_len,
                                                   uint16_t prdt_offset,
                                                   uint16_t prdt_entry_count) {
  return dut_->GetTransferRequestProcessor().FillDescriptorAndSendRequest(
      slot, ddir, resp_offset, resp_len, prdt_offset, prdt_entry_count);
}

zx::result<> UfsTest::MapVmo(zx::unowned_vmo &vmo, fzl::VmoMapper &mapper, uint64_t offset,
                             uint64_t length) {
  if (zx_status_t status = mapper.Map(*vmo, offset, length); status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map IO buffer: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

uint8_t UfsTest::GetSlotStateCount(SlotState slot_state) {
  uint8_t count = 0;
  for (uint8_t slot_num = 0;
       slot_num < dut_->GetTransferRequestProcessor().request_list_.GetSlotCount(); ++slot_num) {
    auto &slot = dut_->GetTransferRequestProcessor().request_list_.GetSlot(slot_num);
    if (slot.state == slot_state) {
      ++count;
    }
  }
  return count;
}

zx::result<uint32_t> UfsTest::ReadAttribute(Attributes attribute, uint8_t index) {
  return dut_->GetDeviceManager().ReadAttribute(attribute, index);
}

zx::result<> UfsTest::WriteAttribute(Attributes attribute, uint32_t value, uint8_t index) {
  return dut_->GetDeviceManager().WriteAttribute(attribute, value, index);
}

zx::result<fdf::MmioBuffer> TestUfs::CreateMmioBuffer(zx_off_t offset, size_t size, zx::vmo vmo) {
  ZX_ASSERT(mock_device_ != nullptr);
  return zx::ok(mock_device_->GetMmioBuffer(std::move(vmo)));
}

}  // namespace ufs

FUCHSIA_DRIVER_EXPORT(ufs::TestUfs);
