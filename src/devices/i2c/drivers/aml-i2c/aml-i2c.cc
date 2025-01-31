// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-i2c.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/logging.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/trace/event.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/threads.h>

#include <soc/aml-common/aml-i2c.h>

#include "aml-i2c-regs.h"

namespace {

constexpr std::string_view kDriverName = "aml-i2c";
constexpr std::string_view kChildNodeName = "aml-i2c";

constexpr zx_signals_t kErrorSignal = ZX_USER_SIGNAL_0;
constexpr zx_signals_t kTxnCompleteSignal = ZX_USER_SIGNAL_1;

constexpr size_t kMaxTransferSize = 512;

zx::result<aml_i2c_delay_values> GetDelay(
    fidl::WireSyncClient<fuchsia_driver_compat::Device>& compat_client) {
  fidl::WireResult metadata = compat_client->GetMetadata();
  if (!metadata.ok()) {
    FDF_LOG(ERROR, "Failed to send GetMetadata request: %s", metadata.status_string());
    return zx::error(metadata.status());
  }
  if (metadata->is_error()) {
    FDF_LOG(ERROR, "Failed to get metadata: %s", zx_status_get_string(metadata->error_value()));
    return metadata->take_error();
  }

  auto* private_metadata =
      std::find_if(metadata->value()->metadata.begin(), metadata->value()->metadata.end(),
                   [](const auto& metadata) { return metadata.type == DEVICE_METADATA_PRIVATE; });

  if (private_metadata == metadata->value()->metadata.end()) {
    FDF_LOG(DEBUG, "Using default delay values: No metadata found");
    return zx::ok(aml_i2c_delay_values{0, 0});
  }

  size_t size;
  auto status = private_metadata->data.get_prop_content_size(&size);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to get_prop_content_size: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  aml_i2c_delay_values delay_values;
  if (size != sizeof delay_values) {
    FDF_LOG(ERROR, "Expected metadata size to be %lu but actual is %lu", sizeof delay_values, size);
    return zx::error(ZX_ERR_INTERNAL);
  }

  status = private_metadata->data.read(&delay_values, 0, size);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to read metadata: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok(delay_values);
}

zx_status_t SetClockDelay(const aml_i2c_delay_values& delay, const fdf::MmioBuffer& regs_iobuff) {
  if (delay.quarter_clock_delay > aml_i2c::Control::kQtrClkDlyMax ||
      delay.clock_low_delay > aml_i2c::TargetAddr::kSclLowDelayMax) {
    zxlogf(ERROR, "invalid clock delay");
    return ZX_ERR_INVALID_ARGS;
  }

  if (delay.quarter_clock_delay > 0) {
    aml_i2c::Control::Get()
        .ReadFrom(&regs_iobuff)
        .set_qtr_clk_dly(delay.quarter_clock_delay)
        .WriteTo(&regs_iobuff);
  }

  if (delay.clock_low_delay > 0) {
    aml_i2c::TargetAddr::Get()
        .FromValue(0)
        .set_scl_low_dly(delay.clock_low_delay)
        .set_use_cnt_scl_low(1)
        .WriteTo(&regs_iobuff);
  }

  return ZX_OK;
}

}  // namespace

namespace aml_i2c {

AmlI2c::AmlI2c(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

void AmlI2c::SetTargetAddr(uint16_t addr) const {
  addr &= 0x7f;
  TargetAddr::Get().ReadFrom(&regs_iobuff()).set_target_address(addr).WriteTo(&regs_iobuff());
}

void AmlI2c::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                       const zx_packet_interrupt_t* interrupt) {
  if (status == ZX_ERR_CANCELED) {
    return;
  }
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to wait for interrupt: %s", zx_status_get_string(status));
    return;
  }
  irq_.ack();
  if (Control::Get().ReadFrom(&regs_iobuff()).error()) {
    event_.signal(0, kErrorSignal);
  } else {
    event_.signal(0, kTxnCompleteSignal);
  }
}

#if 0
zx_status_t AmlI2c::DumpState() {
  printf("control reg      : %08x\n", regs_iobuff().Read32(kControlReg));
  printf("target addr reg  : %08x\n", regs_iobuff().Read32(kTargetAddrReg));
  printf("token list0 reg  : %08x\n", regs_iobuff().Read32(kTokenList0Reg));
  printf("token list1 reg  : %08x\n", regs_iobuff().Read32(kTokenList1Reg));
  printf("token wdata0     : %08x\n", regs_iobuff().Read32(kWriteData0Reg));
  printf("token wdata1     : %08x\n", regs_iobuff().Read32(kWriteData1Reg));
  printf("token rdata0     : %08x\n", regs_iobuff().Read32(kReadData0Reg));
  printf("token rdata1     : %08x\n", regs_iobuff().Read32(kReadData1Reg));

  return ZX_OK;
}
#endif

void AmlI2c::StartXfer() const {
  // First have to clear the start bit before setting (RTFM)
  Control::Get()
      .ReadFrom(&regs_iobuff())
      .set_start(0)
      .WriteTo(&regs_iobuff())
      .set_start(1)
      .WriteTo(&regs_iobuff());
}

zx_status_t AmlI2c::WaitTransferComplete() const {
  constexpr zx_signals_t kSignalMask = kTxnCompleteSignal | kErrorSignal;

  uint32_t observed;
  zx_status_t status = event_.wait_one(kSignalMask, zx::deadline_after(timeout_), &observed);
  if (status != ZX_OK) {
    return status;
  }
  event_.signal(observed, 0);
  if (observed & kErrorSignal) {
    return ZX_ERR_TIMED_OUT;
  }
  return ZX_OK;
}

zx_status_t AmlI2c::Write(cpp20::span<uint8_t> src, const bool stop) const {
  TRACE_DURATION("i2c", "aml-i2c Write");
  ZX_DEBUG_ASSERT(src.size() <= kMaxTransferSize);

  TokenList tokens = TokenList::Get().FromValue(0);
  tokens.Push(TokenList::Token::kStart);
  tokens.Push(TokenList::Token::kTargetAddrWr);

  auto remaining = src.size();
  auto offset = 0;
  while (remaining > 0) {
    const bool is_last_iter = remaining <= WriteData::kMaxWriteBytesPerTransfer;
    const size_t tx_size = is_last_iter ? remaining : WriteData::kMaxWriteBytesPerTransfer;
    for (uint32_t i = 0; i < tx_size; i++) {
      tokens.Push(TokenList::Token::kData);
    }

    if (is_last_iter && stop) {
      tokens.Push(TokenList::Token::kStop);
    }

    tokens.WriteTo(&regs_iobuff());

    WriteData wdata = WriteData::Get().FromValue(0);
    for (uint32_t i = 0; i < tx_size; i++) {
      wdata.Push(src[offset + i]);
    }

    wdata.WriteTo(&regs_iobuff());

    StartXfer();
    // while (Control::Get().ReadFrom(&regs_iobuff()).status()) ;;    // wait for idle
    zx_status_t status = WaitTransferComplete();
    if (status != ZX_OK) {
      return status;
    }

    remaining -= tx_size;
    offset += tx_size;
  }

  return ZX_OK;
}

zx_status_t AmlI2c::Read(cpp20::span<uint8_t> dst, const bool stop) const {
  ZX_DEBUG_ASSERT(dst.size() <= kMaxTransferSize);
  TRACE_DURATION("i2c", "aml-i2c Read");

  TokenList tokens = TokenList::Get().FromValue(0);
  tokens.Push(TokenList::Token::kStart);
  tokens.Push(TokenList::Token::kTargetAddrRd);

  size_t remaining = dst.size();
  size_t offset = 0;
  while (remaining > 0) {
    const bool is_last_iter = remaining <= ReadData::kMaxReadBytesPerTransfer;
    const size_t rx_size = is_last_iter ? remaining : ReadData::kMaxReadBytesPerTransfer;

    for (uint32_t i = 0; i < (rx_size - 1); i++) {
      tokens.Push(TokenList::Token::kData);
    }
    if (is_last_iter) {
      tokens.Push(TokenList::Token::kDataLast);
      if (stop) {
        tokens.Push(TokenList::Token::kStop);
      }
    } else {
      tokens.Push(TokenList::Token::kData);
    }

    tokens.WriteTo(&regs_iobuff());

    // clear registers to prevent data leaking from last xfer
    ReadData rdata = ReadData::Get().FromValue(0).WriteTo(&regs_iobuff());

    StartXfer();

    zx_status_t status = WaitTransferComplete();
    if (status != ZX_OK) {
      return status;
    }

    // while (Control::Get().ReadFrom(&regs_iobuff()).status()) ;;    // wait for idle

    rdata.ReadFrom(&regs_iobuff());

    for (size_t i = 0; i < rx_size; i++) {
      dst[offset + i] = rdata.Pop();
    }

    remaining -= rx_size;
    offset += rx_size;
  }

  return ZX_OK;
}

zx_status_t AmlI2c::StartIrqThread() {
  const char* kRoleName = "fuchsia.devices.i2c.drivers.aml-i2c.interrupt";
  zx::result dispatcher = fdf::SynchronizedDispatcher::Create(
      {}, kRoleName,
      [this](fdf_dispatcher_t*) {
        async::PostTask(driver_dispatcher()->async_dispatcher(), [this]() {
          if (completer_.has_value()) {
            (*std::move(completer_))(zx::ok());
          } else {
            FDF_LOG(ERROR, "Irq thread dispatcher prematurely shutdown.");
          }
        });
      },
      kRoleName);
  if (dispatcher.is_error()) {
    FDF_LOG(ERROR, "Failed to create dispatcher: %s", dispatcher.status_string());
    return dispatcher.status_value();
  }
  irq_dispatcher_.emplace(std::move(dispatcher.value()));

  irq_handler_.set_object(irq_.get());
  irq_handler_.Begin(irq_dispatcher_->async_dispatcher());

  return ZX_OK;
}

void AmlI2c::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_i2cimpl::Device> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  zxlogf(ERROR, "Unknown method %lu", metadata.method_ordinal);
}

void AmlI2c::GetMaxTransferSize(fdf::Arena& arena, GetMaxTransferSizeCompleter::Sync& completer) {
  completer.buffer(arena).ReplySuccess(kMaxTransferSize);
}

void AmlI2c::SetBitrate(SetBitrateRequestView request, fdf::Arena& arena,
                        SetBitrateCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void AmlI2c::Transact(TransactRequestView request, fdf::Arena& arena,
                      TransactCompleter::Sync& completer) {
  TRACE_DURATION("i2c", "aml-i2c Transact");
  for (const auto& op : request->op) {
    if ((op.type.is_read_size() && op.type.read_size() > kMaxTransferSize) ||
        (op.type.is_write_data() && op.type.write_data().count() > kMaxTransferSize)) {
      completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
      return;
    }
  }

  std::vector<fuchsia_hardware_i2cimpl::wire::ReadData> reads;
  for (const auto& op : request->op) {
    SetTargetAddr(op.address);

    zx_status_t status;
    if (op.type.is_read_size()) {
      if (op.type.read_size() > 0) {
        auto dst = fidl::VectorView<uint8_t>{arena, op.type.read_size()};
        status = Read(dst.get(), op.stop);
        reads.push_back({dst});
      } else {
        // Avoid allocating an empty vector because allocating 0 bytes causes an asan error.
        status = Read({}, op.stop);
        reads.push_back({});
      }
    } else {
      status = Write(op.type.write_data().get(), op.stop);
    }
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
  }

  if (reads.empty()) {
    // Avoid allocating an empty vector because allocating 0 bytes causes an asan error.
    completer.buffer(arena).ReplySuccess({});
  } else {
    completer.buffer(arena).ReplySuccess({arena, reads});
  }
}

zx::result<> AmlI2c::Start() {
  // Initialize our compat server.
  {
    // TODO(b/385164506): Don't forward DEVICE_METADATA_I2C_CHANNELS once no longer referenced.
    zx::result<> result =
        device_server_.Initialize(incoming(), outgoing(), node_name(), kChildNodeName,
                                  compat::ForwardMetadata::Some({DEVICE_METADATA_I2C_CHANNELS}));
    if (result.is_error()) {
      return result.take_error();
    }
  }

  zx::result compat_result = incoming()->Connect<fuchsia_driver_compat::Service::Device>();
  if (compat_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to compat service: %s", compat_result.status_string());
    return compat_result.take_error();
  }
  auto compat_client = fidl::WireSyncClient(std::move(compat_result.value()));

  fdf::PDev pdev;
  {
    zx::result result =
        incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>("pdev");
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to pdev protocol: %s", result.status_string());
      return result.take_error();
    }
    pdev = fdf::PDev{std::move(result.value())};
  }

  if (auto mmio = MapMmio(pdev); mmio.is_error()) {
    return mmio.take_error();
  } else {
    regs_iobuff_.emplace(*std::move(mmio));
  }

  zx::result delay = GetDelay(compat_client);
  if (delay.is_error()) {
    FDF_LOG(ERROR, "Failed to get delay values");
    return delay.take_error();
  }

  zx_status_t status = SetClockDelay(delay.value(), regs_iobuff());
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to set clock delay: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  {
    zx::result interrupt = pdev.GetInterrupt(0);
    if (interrupt.is_error()) {
      FDF_LOG(ERROR, "Failed to get interrupt: %s", interrupt.status_string());
      return interrupt.take_error();
    }
    irq_ = std::move(interrupt.value());
  }

  {
    zx::result metadata = pdev.GetFidlMetadata<fuchsia_hardware_i2c_businfo::I2CBusMetadata>(
        fuchsia_hardware_i2c_businfo::I2CBusMetadata::kSerializableName);
    if (metadata.is_error()) {
      FDF_LOG(ERROR, "Failed to get metadata: %s", metadata.status_string());
      return metadata.take_error();
    }
    if (zx::result result = metadata_server_.SetMetadata(metadata.value()); result.is_error()) {
      FDF_LOG(ERROR, "Failed to set metadata for metadata server: %s", result.status_string());
      return result.take_error();
    }
    if (zx::result result = metadata_server_.Serve(*outgoing(), dispatcher()); result.is_error()) {
      FDF_LOG(ERROR, "Failed to serve metadata: %s", result.status_string());
      return result.take_error();
    }
  }

  status = zx::event::create(0, &event_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "zx_event_create failed: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  status = ServeI2cImpl();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to serve i2c impl fidl protocol: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  status = StartIrqThread();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to start irq thread: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  status = CreateChildNode();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to create aml-i2c child node: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

void AmlI2c::PrepareStop(fdf::PrepareStopCompleter completer) {
  if (!irq_dispatcher_.has_value()) {
    completer(zx::ok());
    return;
  }
  completer_.emplace(std::move(completer));
  irq_dispatcher_->ShutdownAsync();
}

zx::result<fdf::MmioBuffer> AmlI2c::MapMmio(fdf::PDev& pdev) { return pdev.MapMmio(0); }

zx_status_t AmlI2c::ServeI2cImpl() {
  auto handler = fuchsia_hardware_i2cimpl::Service::InstanceHandler(
      {.device = i2cimpl_bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                                 fidl::kIgnoreBindingClosure)});

  zx::result result = outgoing()->AddService<fuchsia_hardware_i2cimpl::Service>(std::move(handler));
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add I2C impl service to outgoing: %s", result.status_string());
    return result.status_value();
  }

  return ZX_OK;
}

zx_status_t AmlI2c::CreateChildNode() {
  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    FDF_LOG(ERROR, "Failed to create controller endpoints: %s",
            controller_endpoints.status_string());
    return controller_endpoints.status_value();
  }

  std::vector<fuchsia_driver_framework::Offer> offers = device_server_.CreateOffers2();
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_i2cimpl::Service>(component::kDefaultInstance));
  offers.push_back(metadata_server_.MakeOffer());

  zx::result child =
      AddChild(kChildNodeName, std::vector<fuchsia_driver_framework::NodeProperty2>{}, offers);
  if (child.is_error()) {
    FDF_LOG(ERROR, "Failed to add child: %s", child.status_string());
    return child.status_value();
  }
  child_controller_.Bind(std::move(child.value()));

  return ZX_OK;
}

const fdf::MmioBuffer& AmlI2c::regs_iobuff() const {
  ZX_ASSERT(regs_iobuff_.has_value());
  return regs_iobuff_.value();
}

}  // namespace aml_i2c

FUCHSIA_DRIVER_EXPORT(aml_i2c::AmlI2c);
