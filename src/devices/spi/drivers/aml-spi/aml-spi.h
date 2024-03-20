// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.registers/cpp/wire.h>
#include <fidl/fuchsia.hardware.spiimpl/cpp/driver/wire.h>
#include <fidl/fuchsia.scheduler/cpp/wire.h>
#include <lib/async/cpp/executor.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fpromise/promise.h>
#include <lib/fzl/pinned-vmo.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/mmio/mmio.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/profile.h>
#include <lib/zx/result.h>

#include <optional>

#include <fbl/array.h>
#include <soc/aml-common/aml-spi.h>

#include "sdk/lib/driver/compat/cpp/device_server.h"
#include "src/lib/vmo_store/vmo_store.h"

namespace spi {

class AmlSpi : public fdf::WireServer<fuchsia_hardware_spiimpl::SpiImpl> {
 public:
  struct OwnedVmoInfo {
    uint64_t offset;
    uint64_t size;
    fuchsia_hardware_sharedmemory::SharedVmoRight rights;
  };

  using SpiVmoStore = vmo_store::VmoStore<vmo_store::HashTableStorage<uint32_t, OwnedVmoInfo>>;

  struct ChipInfo {
    ChipInfo() : registered_vmos(vmo_store::Options{}) {}
    ~ChipInfo() = default;

    fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> gpio;
    std::optional<SpiVmoStore> registered_vmos;
  };

  // DmaBuffer holds a contiguous VMO that is both pinned and mapped.
  struct DmaBuffer {
    static zx_status_t Create(const zx::bti& bti, size_t size, DmaBuffer* out_dma_buffer);

    zx::vmo vmo;
    fzl::PinnedVmo pinned;
    fzl::VmoMapper mapped;
  };

  AmlSpi(fdf::MmioBuffer mmio, fidl::ClientEnd<fuchsia_hardware_registers::Device> reset,
         fidl::ClientEnd<fuchsia_scheduler::ProfileProvider> profile_provider, uint32_t reset_mask,
         fbl::Array<ChipInfo> chips, zx::interrupt interrupt,
         const amlogic_spi::amlspi_config_t& config, zx::bti bti, DmaBuffer tx_buffer,
         DmaBuffer rx_buffer)
      : mmio_(std::move(mmio)),
        reset_(std::move(reset)),
        dispatcher_(fdf::Dispatcher::GetCurrent()),
        profile_provider_(std::move(profile_provider), dispatcher_->async_dispatcher()),
        reset_mask_(reset_mask),
        chips_(std::move(chips)),
        interrupt_(std::move(interrupt)),
        config_(config),
        bti_(std::move(bti)),
        tx_buffer_(std::move(tx_buffer)),
        rx_buffer_(std::move(rx_buffer)),
        outgoing_(fdf::Dispatcher::GetCurrent()->get()) {
    bindings_.set_empty_set_handler(fit::bind_member<&AmlSpi::OnAllClientsUnbound>(this));
  }

  void InitRegisters();

  void Serve(fdf::ServerEnd<fuchsia_hardware_spiimpl::SpiImpl> request);

  void Stop();

 private:
  void GetChipSelectCount(fdf::Arena& arena,
                          GetChipSelectCountCompleter::Sync& completer) override {
    completer.buffer(arena).Reply(chips_.size());
  }
  void TransmitVector(fuchsia_hardware_spiimpl::wire::SpiImplTransmitVectorRequest* request,
                      fdf::Arena& arena, TransmitVectorCompleter::Sync& completer) override;
  void ReceiveVector(fuchsia_hardware_spiimpl::wire::SpiImplReceiveVectorRequest* request,
                     fdf::Arena& arena, ReceiveVectorCompleter::Sync& completer) override;
  void ExchangeVector(fuchsia_hardware_spiimpl::wire::SpiImplExchangeVectorRequest* request,
                      fdf::Arena& arena, ExchangeVectorCompleter::Sync& completer) override;
  void RegisterVmo(fuchsia_hardware_spiimpl::wire::SpiImplRegisterVmoRequest* request,
                   fdf::Arena& arena, RegisterVmoCompleter::Sync& completer) override;
  void UnregisterVmo(fuchsia_hardware_spiimpl::wire::SpiImplUnregisterVmoRequest* request,
                     fdf::Arena& arena, UnregisterVmoCompleter::Sync& completer) override;
  void ReleaseRegisteredVmos(
      fuchsia_hardware_spiimpl::wire::SpiImplReleaseRegisteredVmosRequest* request,
      fdf::Arena& arena, ReleaseRegisteredVmosCompleter::Sync& completer) override;
  void TransmitVmo(fuchsia_hardware_spiimpl::wire::SpiImplTransmitVmoRequest* request,
                   fdf::Arena& arena, TransmitVmoCompleter::Sync& completer) override;
  void ReceiveVmo(fuchsia_hardware_spiimpl::wire::SpiImplReceiveVmoRequest* request,
                  fdf::Arena& arena, ReceiveVmoCompleter::Sync& completer) override;
  void ExchangeVmo(fuchsia_hardware_spiimpl::wire::SpiImplExchangeVmoRequest* request,
                   fdf::Arena& arena, ExchangeVmoCompleter::Sync& completer) override;

  void LockBus(fuchsia_hardware_spiimpl::wire::SpiImplLockBusRequest* request, fdf::Arena& arena,
               LockBusCompleter::Sync& completer) override {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void UnlockBus(fuchsia_hardware_spiimpl::wire::SpiImplUnlockBusRequest* request,
                 fdf::Arena& arena, UnlockBusCompleter::Sync& completer) override {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void DumpState();

  zx::result<> Exchange(uint32_t cs, const uint8_t* txdata, uint8_t* out_rxdata,
                        size_t exchange_size);

  void Exchange8(const uint8_t* txdata, uint8_t* out_rxdata, size_t size);
  void Exchange64(const uint8_t* txdata, uint8_t* out_rxdata, size_t size);

  void SetThreadProfile();

  void WaitForTransferComplete();
  void WaitForDmaTransferComplete();

  // Checks size against the registered VMO size and returns a Span with offset applied. Returns a
  // Span with data set to nullptr if vmo_id wasn't found. Returns a Span with size set to zero if
  // offset and/or size are invalid.
  zx::result<cpp20::span<uint8_t>> GetVmoSpan(
      uint32_t chip_select, const fuchsia_hardware_sharedmemory::wire::SharedVmoBuffer& buffer,
      fuchsia_hardware_sharedmemory::SharedVmoRight right);

  zx_status_t ExchangeDma(const uint8_t* txdata, uint8_t* out_rxdata, uint64_t size);

  size_t DoDmaTransfer(size_t words_remaining);

  bool UseDma(size_t size) const;

  void OnAllClientsUnbound();

  const fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>& gpio(uint32_t chip_select) {
    return chips_[chip_select].gpio;
  }

  std::optional<SpiVmoStore>& registered_vmos(uint32_t chip_select) {
    return chips_[chip_select].registered_vmos;
  }

  fdf::MmioBuffer mmio_;
  fidl::WireSyncClient<fuchsia_hardware_registers::Device> reset_;
  fdf::UnownedDispatcher dispatcher_;
  fidl::WireClient<fuchsia_scheduler::ProfileProvider> profile_provider_;
  const uint32_t reset_mask_;
  const fbl::Array<ChipInfo> chips_;
  bool need_reset_ = false;
  zx::interrupt interrupt_;
  const amlogic_spi::amlspi_config_t config_;
  bool apply_scheduler_role_ = true;
  zx::bti bti_;
  DmaBuffer tx_buffer_;
  DmaBuffer rx_buffer_;
  std::vector<uint8_t> rx_vector_buffer_;
  bool shutdown_ = false;
  fdf::OutgoingDirectory outgoing_;
  fdf::ServerBindingGroup<fuchsia_hardware_spiimpl::SpiImpl> bindings_;
};

// AmlSpiDriver is a helper class that is responsible for acquiring resources on behalf of AmlSpi so
// that it can support RAII in DFv2. It implements the driver Start hook, and forwards Stop to the
// AmlSpi instance.
class AmlSpiDriver : public fdf::DriverBase {
 public:
  AmlSpiDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("aml-spi", std::move(start_args), std::move(dispatcher)),
        executor_(fdf::DriverBase::dispatcher()) {}

  void Start(fdf::StartCompleter completer) override;

  void Stop() override {
    if (device_) {
      device_->Stop();
    }
  }

 protected:
  // MapMmio can be overridden by a test in order to provide an fdf::MmioBuffer backed by a fake.
  virtual fpromise::promise<fdf::MmioBuffer, zx_status_t> MapMmio(
      fidl::WireClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t mmio_id);

 private:
  void OnCompatServerInitialized(fdf::StartCompleter completer);
  void AddNode(fdf::MmioBuffer mmio, const amlogic_spi::amlspi_config_t& config,
               zx::interrupt interrupt, zx::bti bti, fdf::StartCompleter completer);

  fpromise::promise<amlogic_spi::amlspi_config_t, zx_status_t> GetConfig();
  fpromise::promise<zx::interrupt, zx_status_t> GetInterrupt();
  fpromise::promise<zx::bti, zx_status_t> GetBti();

  fbl::Array<AmlSpi::ChipInfo> InitChips(const amlogic_spi::amlspi_config_t& config);
  zx::result<compat::DeviceServer::GenericProtocol> GetBanjoProto(compat::BanjoProtoId id);

  fidl::WireClient<fuchsia_driver_framework::Node> parent_;
  fidl::WireClient<fuchsia_driver_framework::NodeController> controller_;
  fidl::WireClient<fuchsia_hardware_platform_device::Device> pdev_;
  fidl::WireClient<fuchsia_driver_compat::Device> compat_;
  compat::AsyncInitializedDeviceServer compat_server_;
  std::unique_ptr<AmlSpi> device_;
  async::Executor executor_;
};

}  // namespace spi
