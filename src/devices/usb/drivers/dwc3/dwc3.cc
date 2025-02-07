// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/dwc3/dwc3.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.usb.dci/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.descriptor/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.endpoint/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/platform-device/cpp/pdev.h>
#include <lib/fit/defer.h>
#include <lib/zx/clock.h>
#include <zircon/syscalls.h>
#include <zircon/threads.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/designware/platform/cpp/bind.h>

#include "src/devices/usb/drivers/dwc3/dwc3-regs.h"

namespace dwc3 {

namespace fdci = fuchsia_hardware_usb_dci;
namespace fdescriptor = fuchsia_hardware_usb_descriptor;
namespace fendpoint = fuchsia_hardware_usb_endpoint;
namespace fpdev = fuchsia_hardware_platform_device;

zx_status_t CacheFlushCommon(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset, size_t length,
                             uint32_t flush_options) {
  if (offset + length < offset || offset + length > buffer->size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  auto virt{reinterpret_cast<uintptr_t>(buffer->virt()) + offset};
  return zx_cache_flush(reinterpret_cast<void*>(virt), length, flush_options);
}

zx_status_t CacheFlush(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset, size_t length) {
  return CacheFlushCommon(buffer, offset, length, ZX_CACHE_FLUSH_DATA);
}

zx_status_t CacheFlushInvalidate(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset,
                                 size_t length) {
  return CacheFlushCommon(buffer, offset, length, ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
}

zx::result<> Dwc3::Start() {
  {  // Compat server initialization.
    auto result = compat_.Initialize(incoming(), outgoing(), node_name(), name(),
                                     compat::ForwardMetadata::All());
    if (result.is_error()) {
      FDF_LOG(ERROR, "compat_.Initalize(): %s", result.status_string());
      return result.take_error();
    }
  }

  if (zx_status_t status = AcquirePDevResources(); status != ZX_OK) {
    FDF_LOG(ERROR, "AcquirePDevResources: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  auto handler = bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure);

  auto serve_result =
      outgoing()->AddService<fdci::UsbDciService>(fdci::UsbDciService::InstanceHandler({
          .device = std::move(handler),
      }));

  if (serve_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add service: %s", serve_result.status_string());
    return serve_result.take_error();
  }

  if (zx_status_t status = Init(); status != ZX_OK) {
    return zx::error(status);
  }

  auto props = std::vector{
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                        bind_fuchsia_designware_platform::BIND_PLATFORM_DEV_VID_DESIGNWARE),
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                        bind_fuchsia_designware_platform::BIND_PLATFORM_DEV_DID_DWC3),
  };

  auto offers = compat_.CreateOffers2();
  offers.push_back(fdf::MakeOffer2<fdci::UsbDciService>());
  offers.push_back(mac_address_metadata_server_.MakeOffer());
  offers.push_back(serial_number_metadata_server_.MakeOffer());

  auto child = AddChild(name(), props, offers);
  if (child.is_error()) {
    FDF_LOG(ERROR, "AddChild(): %s", child.status_string());
    return child.take_error();
  }
  child_.Bind(std::move(*child));

  return zx::ok();
}

zx_status_t Dwc3::AcquirePDevResources() {
  auto pdev = incoming()->Connect<fpdev::Service::Device>("pdev");
  if (pdev.is_error()) {
    FDF_LOG(ERROR, "fidl::CreateEndpoints<fpdev::Service>(): %s", pdev.status_string());
    return pdev.error_value();
  }
  pdev_ = fdf::PDev{std::move(*pdev)};

  if (!pdev_.is_valid()) {
    FDF_LOG(ERROR, "Could not get platform device protocol");
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Initialize mac address metadata server.
  {
    zx::result metadata = pdev_.GetFidlMetadata<fuchsia_boot_metadata::MacAddressMetadata>(
        fuchsia_boot_metadata::MacAddressMetadata::kSerializableName);
    if (metadata.is_ok()) {
      if (zx::result result = mac_address_metadata_server_.SetMetadata(metadata.value());
          result.is_error()) {
        FDF_LOG(ERROR, "Failed to set metadata for mac address metdadata server: %s",
                result.status_string());
        return result.status_value();
      }
    } else if (metadata.status_value() == ZX_ERR_NOT_FOUND) {
      FDF_LOG(DEBUG, "Not forwarding mac address: Metadata not found");
    } else {
      FDF_LOG(ERROR, "Failed to get mac address metadata: %s", metadata.status_string());
      return metadata.status_value();
    }
  }
  if (zx::result result = mac_address_metadata_server_.Serve(*outgoing(), dispatcher());
      result.is_error()) {
    FDF_LOG(ERROR, "Failed to serve mac address metadata: %s", result.status_string());
    return result.status_value();
  }

  // Initialize serial number metadata server.
  {
    zx::result metadata = pdev_.GetFidlMetadata<fuchsia_boot_metadata::SerialNumberMetadata>(
        fuchsia_boot_metadata::SerialNumberMetadata::kSerializableName);
    if (metadata.is_ok()) {
      if (zx::result result = serial_number_metadata_server_.SetMetadata(metadata.value());
          result.is_error()) {
        FDF_LOG(ERROR, "Failed to set metadata for serial number metdadata server: %s",
                result.status_string());
        return result.status_value();
      }
    } else if (metadata.status_value() == ZX_ERR_NOT_FOUND) {
      FDF_LOG(DEBUG, "Not forwarding serial number: Metadata not found");
    } else {
      FDF_LOG(ERROR, "Failed to get serial number metadata: %s", metadata.status_string());
      return metadata.status_value();
    }
  }
  if (zx::result result = serial_number_metadata_server_.Serve(*outgoing(), dispatcher());
      result.is_error()) {
    FDF_LOG(ERROR, "Failed to serve serial number metadata: %s", result.status_string());
    return result.status_value();
  }

  auto mmio = pdev_.MapMmio(0);
  if (mmio.is_error()) {
    FDF_LOG(ERROR, "MapMmio failed: %s", mmio.status_string());
    return mmio.error_value();
  }
  mmio_ = std::move(*mmio);

  auto bti = pdev_.GetBti(0);
  if (bti.is_error()) {
    FDF_LOG(ERROR, "GetBti failed: %s", bti.status_string());
    return bti.error_value();
  }
  bti_ = std::move(*bti);

  auto irq = pdev_.GetInterrupt(0, 0);
  if (irq.is_error()) {
    FDF_LOG(ERROR, "GetInterrupt failed: %s", irq.status_string());
    return irq.error_value();
  }
  irq_ = std::move(*irq);

  if (zx_status_t status = zx::port::create(ZX_PORT_BIND_TO_INTERRUPT, &irq_port_);
      status != ZX_OK) {
    FDF_LOG(ERROR, "zx::port::create failed: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = irq_.bind(irq_port_, 0, ZX_INTERRUPT_BIND); status != ZX_OK) {
    FDF_LOG(ERROR, "irq bind to port failed: %s", zx_status_get_string(status));
    return status;
  }
  irq_bound_to_port_ = true;

  return ZX_OK;
}

zx_status_t Dwc3::Init() {
  // Start by identifying our hardware and making sure that we recognize it, and
  // it is a version that we know we can support.  Then, reset the hardware so
  // that we know it is in a good state.
  uint32_t ep_count{0};
  {
    std::lock_guard<std::mutex> lock(lock_);

    // Now that we have our registers, check to make sure that we are running on
    // a version of the hardware that we support.
    if (zx_status_t status = CheckHwVersion(); status != ZX_OK) {
      FDF_LOG(ERROR, "CheckHwVersion failed: %s", zx_status_get_string(status));
      return status;
    }

    // Now that we have our registers, reset the hardware.  This will ensure that
    // we are starting from a known state moving forward.
    if (zx_status_t status = ResetHw(); status != ZX_OK) {
      FDF_LOG(ERROR, "HW Reset Failed: %s", zx_status_get_string(status));
      return status;
    }

    // Finally, figure out the number of endpoints that this version of the
    // controller supports.
    ep_count = GHWPARAMS3::Get().ReadFrom(get_mmio()).DWC_USB31_NUM_EPS();
  }

  if (ep_count < (kUserEndpointStartNum + 1)) {
    FDF_LOG(ERROR, "HW supports only %u physical endpoints, but at least %u are needed to operate.",
            ep_count, (kUserEndpointStartNum + 1));
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Now go ahead and allocate the user endpoint storage and uep servers.
  user_endpoints_.Init(ep_count - kUserEndpointStartNum, bti_, this);

  // Now that we have our BTI, and have reset our hardware, we can go ahead and
  // release the quarantine on any pages which may have been previously pinned
  // by this BTI.
  if (zx_status_t status = bti_.release_quarantine(); status != ZX_OK) {
    FDF_LOG(ERROR, "Release quarantine failed: %s", zx_status_get_string(status));
    return status;
  }

  // If something goes wrong after this point, make sure to release any of our
  // allocated dma buffers.
  auto cleanup = fit::defer([this]() { ReleaseResources(); });

  // Strictly speaking, we should not need RW access to this buffer.
  // Unfortunately, attempting to writeback and invalidate the cache before
  // reading anything from the buffer produces a page fault right if this buffer
  // is mapped read only, so for now, we keep the buffer mapped RW.
  zx_status_t status = dma_buffer::CreateBufferFactory()->CreateContiguous(bti_, kEventBufferSize,
                                                                           12, &event_buffer_);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "dma_buffer init fails: %s", zx_status_get_string(status));
    return status;
  }
  CacheFlushInvalidate(event_buffer_.get(), 0, kEventBufferSize);

  // Now that we have allocated our event buffer, we have at least one region
  // pinned.  We need to be sure to place the hardware into reset before
  // unpinning the memory during shutdown.
  has_pinned_memory_ = true;

  {
    std::lock_guard<std::mutex> lock(ep0_.lock);
    zx_status_t status =
        dma_buffer::CreateBufferFactory()->CreateContiguous(bti_, kEp0BufferSize, 12, &ep0_.buffer);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "ep0_buffer init failed: %s", zx_status_get_string(status));
      return status;
    }
  }

  if (zx_status_t status = Ep0Init(); status != ZX_OK) {
    FDF_LOG(ERROR, "Ep0Init init failed: %s", zx_status_get_string(status));
    return status;
  }

  // Things went well.  Cancel our cleanup routine.
  cleanup.cancel();
  init_done_.Signal();  // Unblock any FIDL clients awaiting Endpoint service.
  return ZX_OK;
}

void Dwc3::ReleaseResources() {
  // The IRQ thread had better not be running at this point.
  ZX_ASSERT(!irq_thread_started_.load());

  // Unbind the interrupt from the interrupt port.
  if (irq_bound_to_port_) {
    irq_.bind(irq_port_, 0, ZX_INTERRUPT_UNBIND);
    irq_bound_to_port_ = false;
  }

  {
    std::lock_guard<std::mutex> lock(lock_);
    // If we managed to get our registers mapped, place the device into reset so
    // we are certain that there is no DMA going on in the background.
    if (mmio_.has_value()) {
      if (zx_status_t status = ResetHw(); status != ZX_OK) {
        // Deliberately panic and terminate this driver if we fail to place the
        // hardware into reset at this point and we have any pinned memory..  We do this
        // deliberately because, if we cannot put the hardware into reset, it may still be accessing
        // pages we previously pinned using DMA.  If we are on a system with no
        // IOMMU, deliberately terminating the process will ensure that our
        // pinned pages are quarantined instead of being returned to the page
        // pool.
        if (has_pinned_memory_) {
          FDF_LOG(
              ERROR,
              "Failed to place HW into reset during shutdown (%s), self-terminating in order to "
              "ensure quarantine",
              zx_status_get_string(status));
          ZX_ASSERT(false);
        }
      }
    }
  }

  // Now go ahead and release any buffers we may have pinned.
  {
    std::lock_guard<std::mutex> lock(ep0_.lock);
    ep0_.out.enabled = false;
    ep0_.in.enabled = false;
    ep0_.buffer.reset();
    ep0_.shared_fifo.Release();
  }

  for (UserEndpoint& uep : user_endpoints_) {
    std::lock_guard<std::mutex> lock(uep.ep.lock);
    uep.fifo.Release();
    uep.ep.enabled = false;
  }

  event_buffer_.reset();
  has_pinned_memory_ = false;
}

zx_status_t Dwc3::CheckHwVersion() {
  auto* mmio = get_mmio();
  const uint32_t core_id = GSNPSID::Get().ReadFrom(mmio).core_id();
  if (core_id == 0x5533) {
    return ZX_OK;
  }

  const uint32_t ip_version = USB31_VER_NUMBER::Get().ReadFrom(mmio).IPVERSION();

  auto is_ascii_digit = [](char val) -> bool { return (val >= '0') && (val <= '9'); };
  auto is_ascii_letter = [](char val) -> bool {
    return ((val >= 'A') && (val <= 'Z')) || ((val >= 'a') && (val <= 'z'));
  };

  const char c1 = static_cast<char>((ip_version >> 24) & 0xFF);
  const char c2 = static_cast<char>((ip_version >> 16) & 0xFF);
  const char c3 = static_cast<char>((ip_version >> 8) & 0xFF);
  const char c4 = static_cast<char>(ip_version & 0xFF);

  // Format defined by section 1.3.44 of the DWC3 Programming Guide
  if (!is_ascii_digit(c1) || !is_ascii_digit(c2) || !is_ascii_digit(c3) ||
      (!is_ascii_letter(c4) && (c4 != '*'))) {
    FDF_LOG(ERROR, "Unrecognized USB IP Version 0x%08x", ip_version);
    return ZX_ERR_NOT_SUPPORTED;
  }

  const int major = c1 - '0';
  const int minor = ((c2 - '0') * 10) + (c3 - '0');

  if (major != 1) {
    FDF_LOG(ERROR, "Unsupported USB IP Version %d.%02d%c", major, minor, c4);
    return ZX_ERR_NOT_SUPPORTED;
  }

  FDF_LOG(INFO, "Detected DWC3 IP version %d.%02d%c", major, minor, c4);
  return ZX_OK;
}

zx_status_t Dwc3::ResetHw() {
  auto* mmio = get_mmio();

  // Clear the run/stop bit and request a software reset.
  DCTL::Get().ReadFrom(mmio).set_RUN_STOP(0).set_CSFTRST(1).WriteTo(mmio);

  // HW will clear the software reset bit when it is finished with the reset
  // process.
  zx::time start = zx::clock::get_monotonic();
  while (DCTL::Get().ReadFrom(mmio).CSFTRST()) {
    if ((zx::clock::get_monotonic() - start) >= kHwResetTimeout) {
      return ZX_ERR_TIMED_OUT;
    }
    usleep(1000);
  }

  return ZX_OK;
}

void Dwc3::SetDeviceAddress(uint32_t address) {
  auto* mmio = get_mmio();
  DCFG::Get().ReadFrom(mmio).set_DEVADDR(address).WriteTo(mmio);
}

void Dwc3::StartPeripheralMode() {
  {
    std::lock_guard<std::mutex> lock(lock_);
    auto* mmio = get_mmio();

    // configure and enable PHYs
    GUSB2PHYCFG::Get(0)
        .ReadFrom(mmio)
        .set_USBTRDTIM(9)    // USB2.0 Turn-around time == 9 phy clocks
        .set_ULPIAUTORES(0)  // No auto resume
        .WriteTo(mmio);

    GUSB3PIPECTL::Get(0)
        .ReadFrom(mmio)
        .set_DELAYP1TRANS(0)
        .set_SUSPENDENABLE(0)
        .set_LFPSFILTER(1)
        .set_SS_TX_DE_EMPHASIS(1)
        .WriteTo(mmio);

    // TODO(johngro): This is the number of receive buffers.  Why do we set it to 16?
    constexpr uint32_t nump = 16;
    DCFG::Get()
        .ReadFrom(mmio)
        .set_NUMP(nump)                  // number of receive buffers
        .set_DEVSPD(DCFG::DEVSPD_SUPER)  // max speed is 5Gbps USB3.1
        .set_DEVADDR(0)                  // device address is 0
        .WriteTo(mmio);

    // Program the location of the event buffer, then enable event delivery.
    StartEvents();
  }

  Ep0Start();

  {
    // Set the run/stop bit to start the controller
    std::lock_guard<std::mutex> lock(lock_);
    auto* mmio = get_mmio();
    DCTL::Get().FromValue(0).set_RUN_STOP(1).WriteTo(mmio);
  }
}

void Dwc3::ResetConfiguration() {
  {
    std::lock_guard<std::mutex> lock(lock_);
    auto* mmio = get_mmio();
    // disable all endpoints except EP0_OUT and EP0_IN
    DALEPENA::Get().FromValue(0).EnableEp(kEp0Out).EnableEp(kEp0In).WriteTo(mmio);
  }

  for (UserEndpoint& uep : user_endpoints_) {
    std::lock_guard<std::mutex> lock(uep.ep.lock);
    EpEndTransfers(uep.ep, ZX_ERR_IO_NOT_PRESENT);
    EpSetStall(uep.ep, false);
  }
}

void Dwc3::HandleResetEvent() {
  FDF_LOG(INFO, "Dwc3::HandleResetEvent");

  Ep0Reset();

  for (UserEndpoint& uep : user_endpoints_) {
    std::lock_guard<std::mutex> lock(uep.ep.lock);
    EpEndTransfers(uep.ep, ZX_ERR_IO_NOT_PRESENT);
    EpSetStall(uep.ep, false);
  }

  {
    std::lock_guard<std::mutex> lock(lock_);
    SetDeviceAddress(0);
  }

  Ep0Start();

  {
    std::lock_guard<std::mutex> lock(dci_lock_);
    if (dci_intf_.is_valid()) {
      DciIntfSetConnected(true);
    }
  }
}

void Dwc3::HandleConnectionDoneEvent() {
  uint16_t ep0_max_packet = 0;
  fdescriptor::wire::UsbSpeed new_speed{fdescriptor::UsbSpeed::kUndefined};

  {
    std::lock_guard<std::mutex> lock(lock_);
    auto* mmio = get_mmio();

    uint32_t speed = DSTS::Get().ReadFrom(mmio).CONNECTSPD();

    switch (speed) {
      case DSTS::CONNECTSPD_HIGH:
        new_speed = fdescriptor::UsbSpeed::kHigh;
        ep0_max_packet = 64;
        break;
      case DSTS::CONNECTSPD_FULL:
        new_speed = fdescriptor::UsbSpeed::kFull;
        ep0_max_packet = 64;
        break;
      case DSTS::CONNECTSPD_SUPER:
        new_speed = fdescriptor::UsbSpeed::kSuper;
        ep0_max_packet = 512;
        break;
      case DSTS::CONNECTSPD_ENHANCED_SUPER:
        new_speed = fdescriptor::UsbSpeed::kEnhancedSuper;
        ep0_max_packet = 512;
        break;
      default:
        FDF_LOG(ERROR, "unsupported speed %u", speed);
        break;
    }
  }

  if (ep0_max_packet) {
    std::lock_guard<std::mutex> lock(ep0_.lock);

    std::array eps{&ep0_.out, &ep0_.in};
    for (Endpoint* ep : eps) {
      ep->type = USB_ENDPOINT_CONTROL;
      ep->interval = 0;
      ep->max_packet_size = ep0_max_packet;
      CmdEpSetConfig(*ep, true);
    }
    ep0_.cur_speed = new_speed;
  }

  {
    std::lock_guard<std::mutex> lock(dci_lock_);
    if (dci_intf_.is_valid()) {
      DciIntfSetSpeed(new_speed);
    }
  }
}

void Dwc3::HandleDisconnectedEvent() {
  FDF_LOG(INFO, "Dwc3::HandleDisconnectedEvent");

  {
    std::lock_guard<std::mutex> ep0_lock(ep0_.lock);
    CmdEpEndTransfer(ep0_.out);
    ep0_.state = Ep0::State::None;
  }

  {
    std::lock_guard<std::mutex> lock(dci_lock_);
    if (dci_intf_.is_valid()) {
      DciIntfSetConnected(false);
    }
  }

  for (UserEndpoint& uep : user_endpoints_) {
    std::lock_guard<std::mutex> lock(uep.ep.lock);
    EpEndTransfers(uep.ep, ZX_ERR_IO_NOT_PRESENT);
    EpSetStall(uep.ep, false);
  }
}

void Dwc3::Stop() {
  if (irq_thread_started_.load()) {
    zx_status_t status = SignalIrqThread(IrqSignal::Exit);
    // if we can't signal the thread, we are not going to be able to shut down
    // and we should just terminate the process instead.
    ZX_ASSERT(status == ZX_OK);
    thrd_join(irq_thread_, nullptr);
    irq_thread_started_.store(false);
  }

  ReleaseResources();
}

void Dwc3::ConnectToEndpoint(ConnectToEndpointRequest& request,
                             ConnectToEndpointCompleter::Sync& completer) {
  zx_status_t status{init_done_.Wait(zx::deadline_after(kEndpointDeadline))};
  if (status == ZX_ERR_TIMED_OUT) {
    FDF_LOG(ERROR, "Init() runtime exceeds deadline");
    completer.Reply(fit::as_error(status));
    return;
  }

  UserEndpoint* uep{get_user_endpoint(UsbAddressToEpNum(request.ep_addr()))};
  if (uep == nullptr || !uep->server.has_value()) {
    completer.Reply(fit::as_error(ZX_ERR_INVALID_ARGS));
    return;
  }

  uep->server->Connect(uep->server->dispatcher(), std::move(request.ep()));
  completer.Reply(fit::ok());
}

void Dwc3::SetInterface(SetInterfaceRequest& request, SetInterfaceCompleter::Sync& completer) {
  if (!request.interface().is_valid()) {
    zxlogf(ERROR, "Interface should be valid");
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  std::lock_guard<std::mutex> lock(dci_lock_);

  if (dci_intf_.is_valid()) {
    FDF_LOG(ERROR, "%s: DCI Interface already set", __func__);
    completer.Reply(zx::error(ZX_ERR_BAD_STATE));
    return;
  }

  dci_intf_.Bind(std::move(request.interface()));
  completer.Reply(zx::ok());
}

void Dwc3::StartController(StartControllerCompleter::Sync& completer) {
  ZX_ASSERT(!irq_thread_started_);
  // Start the interrupt thread.
  auto irq_thunk = +[](void* arg) -> int { return static_cast<Dwc3*>(arg)->IrqThread(); };
  if (int rc = thrd_create_with_name(&irq_thread_, irq_thunk, static_cast<void*>(this),
                                     "dwc3-interrupt-thread");
      rc != thrd_success) {
    completer.Reply(zx::error(ZX_ERR_INTERNAL));
    return;
  }
  irq_thread_started_.store(true);

  StartPeripheralMode();
  completer.Reply(zx::ok());
}

void Dwc3::StopController(StopControllerCompleter::Sync& completer) {
  Ep0Reset();
  ep0_.Reset();
  for (UserEndpoint& uep : user_endpoints_) {
    CancelAll(uep.ep);
    uep.Reset();
  }

  if (irq_thread_started_.load()) {
    zx_status_t status = SignalIrqThread(IrqSignal::Exit);
    // if we can't signal the thread, we are not going to be able to shut down
    // and we should just terminate the process instead.
    ZX_ASSERT(status == ZX_OK);
    thrd_join(irq_thread_, nullptr);
    irq_thread_started_.store(false);
  }

  zx_status_t status;
  {
    std::lock_guard<std::mutex> _(lock_);
    status = ResetHw();
  }
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to reset hardware %s", zx_status_get_string(status));
    completer.Reply(zx::error(status));
    return;
  }
  zx::nanosleep(zx::deadline_after(zx::msec(50)));
  completer.Reply(zx::ok());
}

void Dwc3::ConfigureEndpoint(ConfigureEndpointRequest& request,
                             ConfigureEndpointCompleter::Sync& completer) {
  const uint8_t ep_num = UsbAddressToEpNum(request.ep_descriptor().b_endpoint_address());
  UserEndpoint* const uep = get_user_endpoint(ep_num);

  if (uep == nullptr) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  uint8_t ep_type = usb_ep_type2(request.ep_descriptor());

  if (ep_type == USB_ENDPOINT_ISOCHRONOUS) {
    FDF_LOG(ERROR, "isochronous endpoints are not supported");
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }

  std::lock_guard<std::mutex> lock(uep->ep.lock);

  if (uep->ep.enabled) {
    // Endpoint already configured, nothing to do.
    completer.Reply(zx::ok());
    return;
  }

  if (zx_status_t status = uep->fifo.Init(bti_); status != ZX_OK) {
    FDF_LOG(ERROR, "fifo init failed %s", zx_status_get_string(status));
    completer.Reply(zx::error(status));
    return;
  }

  uep->ep.max_packet_size = usb_ep_max_packet2(request.ep_descriptor());
  uep->ep.type = ep_type;
  uep->ep.interval = request.ep_descriptor().b_interval();
  // TODO(voydanoff) USB3 support

  uep->ep.enabled = true;
  EpSetConfig(uep->ep, true);

  // TODO(johngro): What protects this configured_ state from a locking/threading perspective?
  if (configured_) {
    UserEpQueueNext(*uep);
  }

  completer.Reply(zx::ok());
}

void Dwc3::DisableEndpoint(DisableEndpointRequest& request,
                           DisableEndpointCompleter::Sync& completer) {
  const uint8_t ep_num = UsbAddressToEpNum(request.ep_address());
  UserEndpoint* const uep = get_user_endpoint(ep_num);

  if (uep == nullptr) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  FidlRequestQueue to_complete;
  {
    std::lock_guard<std::mutex> lock(uep->ep.lock);
    to_complete = CancelAllLocked(uep->ep);
    uep->fifo.Release();
    uep->ep.enabled = false;
  }

  to_complete.CompleteAll(ZX_ERR_IO_NOT_PRESENT, 0);
  completer.Reply(zx::ok());
}

void Dwc3::EndpointSetStall(EndpointSetStallRequest& request,
                            EndpointSetStallCompleter::Sync& completer) {
  const uint8_t ep_num = UsbAddressToEpNum(request.ep_address());
  UserEndpoint* const uep = get_user_endpoint(ep_num);

  if (uep == nullptr) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  std::lock_guard<std::mutex> lock(uep->ep.lock);
  if (zx_status_t status = EpSetStall(uep->ep, true); status != ZX_OK) {
    completer.Reply(zx::error(status));
  } else {
    completer.Reply(zx::ok());
  }
}

void Dwc3::EndpointClearStall(EndpointClearStallRequest& request,
                              EndpointClearStallCompleter::Sync& completer) {
  const uint8_t ep_num = UsbAddressToEpNum(request.ep_address());
  UserEndpoint* const uep = get_user_endpoint(ep_num);

  if (uep == nullptr) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  std::lock_guard<std::mutex> lock(uep->ep.lock);
  if (zx_status_t status = EpSetStall(uep->ep, false); status != ZX_OK) {
    completer.Reply(zx::error(status));
  } else {
    completer.Reply(zx::ok());
  }
}

zx::result<> Dwc3::CommonCancelAll(uint8_t ep_addr) {
  const uint8_t ep_num = UsbAddressToEpNum(ep_addr);
  UserEndpoint* const uep = get_user_endpoint(ep_num);

  if (uep == nullptr) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (zx_status_t status = CancelAll(uep->ep); status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok();
}

void Dwc3::CancelAll(CancelAllRequest& request, CancelAllCompleter::Sync& completer) {
  completer.Reply(CommonCancelAll(request.ep_address()));
}

void Dwc3::DciIntfSetSpeed(fdescriptor::wire::UsbSpeed speed) {
  ZX_ASSERT(dci_intf_.is_valid());

  fidl::Arena arena;
  auto result = dci_intf_.buffer(arena)->SetSpeed(speed);
  if (!result.ok()) {
    FDF_LOG(ERROR, "(framework) SetSpeed(): %s", result.status_string());
  } else if (result->is_error()) {
    FDF_LOG(ERROR, "SetSpeed(): %s", zx_status_get_string(result->error_value()));
  }
}

void Dwc3::DciIntfSetConnected(bool connected) {
  ZX_ASSERT(dci_intf_.is_valid());

  fidl::Arena arena;
  auto result = dci_intf_.buffer(arena)->SetConnected(connected);
  if (!result.ok()) {
    FDF_LOG(ERROR, "(framework) SetConnected(): %s", result.status_string());
  } else if (result->is_error()) {
    FDF_LOG(ERROR, "SetConnected(): %s", zx_status_get_string(result->error_value()));
  }
}

zx::result<size_t> Dwc3::DciIntfControl(const fdescriptor::wire::UsbSetup* setup,
                                        const uint8_t* write_buffer, size_t write_size,
                                        uint8_t* read_buffer, size_t read_size) {
  ZX_ASSERT(dci_intf_.is_valid());

  auto fwrite =
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(write_buffer), write_size);

  fidl::Arena arena;
  auto result = dci_intf_.buffer(arena)->Control(*setup, fwrite);

  if (!result.ok()) {
    FDF_LOG(ERROR, "(framework) Control(): %s", result.status_string());
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Control(): %s", zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  // A lightweight byte-span is used to make it easier to process the read data.
  cpp20::span<uint8_t> read_data{result.value()->read.get()};

  // Don't blow out caller's buffer.
  if (read_data.size_bytes() > read_size) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  size_t read_actual{0};

  if (!read_data.empty()) {
    std::memcpy(read_buffer, read_data.data(), read_data.size_bytes());
    read_actual = read_data.size_bytes();
  }

  return zx::ok(read_actual);
}

void Dwc3::EpServer::GetInfo(GetInfoCompleter::Sync& completer) {
  auto info{fendpoint::EndpointInfo::WithControl(fendpoint::ControlEndpointInfo{})};

  switch (uep_->ep.type) {
    case USB_ENDPOINT_CONTROL:
      // Set up above.
      break;
    case USB_ENDPOINT_ISOCHRONOUS: {
      fendpoint::IsochronousEndpointInfo isoc;
      isoc.lead_time(1);
      info.isochronous(std::move(isoc));
      break;
    }
    case USB_ENDPOINT_BULK:
      info.bulk(fendpoint::BulkEndpointInfo{});
      break;
    case USB_ENDPOINT_INTERRUPT:
      info.interrupt(fendpoint::InterruptEndpointInfo{});
      break;
    default:
      // In theory, this should never happen unless a new EP type is added to the spec.
      FDF_LOG(ERROR, "unknown usb endpoint type: 0x%xd", uep_->ep.type);
      completer.Reply(zx::error(ZX_ERR_BAD_STATE));
  }

  completer.Reply(zx::ok(std::move(info)));
}

void Dwc3::EpServer::QueueRequests(QueueRequestsRequest& request,
                                   QueueRequestsCompleter::Sync& completer) {
  for (auto& req : request.req()) {
    std::lock_guard<std::mutex> lock(uep_->ep.lock);

    usb::FidlRequest freq{std::move(req)};

    zx_status_t status{ZX_OK};

    if (!uep_->ep.enabled) {
      status = ZX_ERR_BAD_STATE;
      FDF_LOG(ERROR, "Dwc3: ep(%u) not enabled!", uep_->ep.ep_num);
    }

    if (status == ZX_OK && freq->data()->size() != 1) {
      status = ZX_ERR_INVALID_ARGS;
      FDF_LOG(ERROR, "scatter-gather not implemented");
    }

    if (status == ZX_OK && uep_->ep.IsOutput()) {
      // Dig the length out of the request data block.
      size_t length{freq->data()->at(0).size().value()};

      if (length == 0 || (length % uep_->ep.max_packet_size) != 0) {
        status = ZX_ERR_INVALID_ARGS;
        FDF_LOG(ERROR, "Dwc3: OUT transfers must be multiple of max packet size (len %ld mps %hu)",
                length, uep_->ep.max_packet_size);
      }
    }

    if (status != ZX_OK) {
      FDF_LOG(ERROR, "failing request with status %s", zx_status_get_string(status));
      RequestComplete(status, 0, std::move(freq));

      if (status = dwc3_->SignalIrqThread(IrqSignal::Wakeup); status != ZX_OK) {
        FDF_LOG(DEBUG, "Failed to signal IRQ thread %s", zx_status_get_string(status));
      }
      continue;
    }

    RequestInfo info{0, 0, uep_, std::move(freq)};
    uep_->ep.queued_reqs.push(std::move(info));

    if (dwc3_->configured_) {
      dwc3_->UserEpQueueNext(*uep_);
    }
  }
}

void Dwc3::EpServer::CancelAll(CancelAllCompleter::Sync& completer) {
  completer.Reply(dwc3_->CommonCancelAll(uep_->ep.ep_num));
}

}  // namespace dwc3

FUCHSIA_DRIVER_EXPORT(dwc3::Dwc3);
