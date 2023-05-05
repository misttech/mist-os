// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "audio-stream-in.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/zx/clock.h>
#include <math.h>
#include <threads.h>
#include <unistd.h>

#include <numeric>
#include <optional>
#include <utility>

#include "src/media/lib/memory_barriers/memory_barriers.h"

namespace {

// kTransferInterval is the timer time. The function of this timer is to send the ring buffer
// position to the HW DSP FW regularly.
constexpr zx::duration kTransferInterval = zx::msec(20);

}  // namespace

namespace audio::aml_g12 {

constexpr size_t kMinSampleRate = 48000;
constexpr size_t kMaxSampleRate = 96000;

AudioStreamInDsp::AudioStreamInDsp(zx_device_t* parent)
    : SimpleAudioStream(parent, true /* is input */) {
  frames_per_second_ = kMinSampleRate;
  status_time_ = inspect().GetRoot().CreateInt("status_time", 0);
  dma_status_ = inspect().GetRoot().CreateUint("dma_status", 0);
  pdm_status_ = inspect().GetRoot().CreateUint("pdm_status", 0);
  ring_buffer_physical_address_ = inspect().GetRoot().CreateUint("ring_buffer_physical_address", 0);
}

int AudioStreamInDsp::Thread() {
  while (1) {
    zx::time timestamp;
    irq_.wait(&timestamp);
    if (!running_.load()) {
      break;
    }
    zxlogf(ERROR, "DMA status: 0x%08X  PDM status: 0x%08X", lib_->GetDmaStatus(),
           lib_->GetPdmStatus());
    status_time_.Set(timestamp.get());
    dma_status_.Set(lib_->GetDmaStatus());
    pdm_status_.Set(lib_->GetPdmStatus());
  }
  zxlogf(INFO, "Exiting interrupt thread");
  return 0;
}

int AudioStreamInDsp::MailboxBind() {
  zx::result client =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_mailbox::Service::Device>("audio-mailbox");
  if (client.is_error()) {
    zxlogf(ERROR, "Failed to connect fidl protocol: %s", client.status_string());
    return client.status_value();
  }

  auto audio_mailbox = fidl::WireSyncClient(std::move(client.value()));

  audio_mailbox_ = std::make_unique<AmlMailboxDevice>(std::move(audio_mailbox));
  if (audio_mailbox_ == nullptr) {
    zxlogf(ERROR, "failed to create Mailbox device");
    return ZX_ERR_NO_MEMORY;
  }
  return ZX_OK;
}

int AudioStreamInDsp::DspBind() {
  zx::result client =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_dsp::Service::Device>("audio-dsp");
  if (client.is_error()) {
    zxlogf(ERROR, "Failed to connect fidl protocol: %s", client.status_string());
    return client.status_value();
  }

  auto audio_dsp = fidl::WireSyncClient(std::move(client.value()));

  audio_dsp_ = std::make_unique<AmlDspDevice>(std::move(audio_dsp));
  if (audio_dsp_ == nullptr) {
    zxlogf(ERROR, "failed to create DSP device");
    return ZX_ERR_NO_MEMORY;
  }

  // Load the DSP firmware that processes PDM audio data.
  zx_status_t status = audio_dsp_->DspHwInit(false);
  if (status != ZX_OK) {
    zxlogf(ERROR, "HW DSP initialization failed: %s", zx_status_get_string(status));
    return status;
  }
  return status;
}

zx_status_t AudioStreamInDsp::Create(void* ctx, zx_device_t* parent) {
  auto stream = audio::SimpleAudioStream::Create<AudioStreamInDsp>(parent);
  if (stream == nullptr) {
    zxlogf(ERROR, "Could not create aml-g12-pdm driver");
    return ZX_ERR_NO_MEMORY;
  }
  [[maybe_unused]] auto unused = fbl::ExportToRawPtr(&stream);
  return ZX_OK;
}

zx_status_t AudioStreamInDsp::SetGain(const audio_proto::SetGainReq& req) {
  return req.gain == 0.f ? ZX_OK : ZX_ERR_INVALID_ARGS;
}

zx_status_t AudioStreamInDsp::Init() {
  auto status = InitPDev();
  if (status != ZX_OK) {
    return status;
  }

  status = AddFormats();
  if (status != ZX_OK) {
    return status;
  }
  // Set our gain capabilities.
  cur_gain_state_.cur_gain = 0;
  cur_gain_state_.cur_mute = false;
  cur_gain_state_.cur_agc = false;
  cur_gain_state_.min_gain = 0;
  cur_gain_state_.max_gain = 0;
  cur_gain_state_.gain_step = 0;
  cur_gain_state_.can_mute = false;
  cur_gain_state_.can_agc = false;

  strncpy(mfr_name_, metadata_.manufacturer, sizeof(mfr_name_));
  strncpy(prod_name_, metadata_.product_name, sizeof(prod_name_));
  unique_id_ = AUDIO_STREAM_UNIQUE_ID_BUILTIN_MICROPHONE;
  snprintf(device_name_, sizeof(device_name_), "%s-audio-pdm-in", prod_name_);

  // TODO(mpuryear): change this to the domain of the clock received from the board driver
  clock_domain_ = 0;

  return ZX_OK;
}

zx_status_t AudioStreamInDsp::InitPDev() {
  size_t actual = 0;
  auto status = device_get_fragment_metadata(parent(), "pdev", DEVICE_METADATA_PRIVATE, &metadata_,
                                             sizeof(metadata::AmlPdmConfig), &actual);
  if (status != ZX_OK || sizeof(metadata::AmlPdmConfig) != actual) {
    zxlogf(ERROR, "device_get_metadata failed %s", zx_status_get_string(status));
    return status;
  }

  zx::result pdev_result = ddk::PDevFidl::Create(parent(), ddk::PDevFidl::kFragmentName);
  if (pdev_result.is_error()) {
    zxlogf(ERROR, "get pdev failed %s", pdev_result.status_string());
    return pdev_result.error_value();
  }

  ddk::PDevFidl pdev = std::move(pdev_result.value());
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "could not get pdev");
    return ZX_ERR_NO_RESOURCES;
  }

  status = pdev.GetBti(0, &bti_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "could not obtain bti %d", status);
    return status;
  }

  std::optional<fdf::MmioBuffer> mmio0, mmio1, mmio2;
  status = pdev.MapMmio(0, &mmio0);
  if (status != ZX_OK) {
    zxlogf(ERROR, "could not map mmio0 %d", status);
    return status;
  }
  status = pdev.MapMmio(1, &mmio1);
  if (status != ZX_OK) {
    zxlogf(ERROR, "could not map mmio1 %d", status);
    return status;
  }

  status = pdev.MapMmio(2, &mmio2);
  if (status != ZX_OK) {
    zxlogf(ERROR, "could not map mmio2 %d", status);
    return status;
  }

  status = pdev.MapMmio(3, &dsp_mmio_, ZX_CACHE_POLICY_CACHED);
  if (status != ZX_OK) {
    zxlogf(ERROR, "unable to get dsp mmio %d", status);
    return status;
  }

  status = pdev.GetInterrupt(0, 0, &irq_);
  if (status != ZX_ERR_OUT_OF_RANGE) {  // Not specified in the board file.
    if (status != ZX_OK) {
      zxlogf(ERROR, "could not get IRQ %d", status);
      return status;
    }

    auto irq_thread = [](void* arg) -> int {
      return reinterpret_cast<AudioStreamInDsp*>(arg)->Thread();
    };
    running_.store(true);
    int rc = thrd_create_with_name(&thread_, irq_thread, reinterpret_cast<void*>(this),
                                   "aml_pdm_irq_thread");
    if (rc != thrd_success) {
      zxlogf(ERROR, "could not create thread %d", rc);
      return status;
    }
  }

  status = MailboxBind();
  if (status != ZX_OK) {
    zxlogf(ERROR, "The mailbox device node binding failed");
    return status;
  }

  status = DspBind();
  if (status != ZX_OK) {
    zxlogf(ERROR, "The dsp device node binding failed");
    return status;
  }

  lib_ = AmlPdmDevice::Create(*std::move(mmio0), *std::move(mmio1), *std::move(mmio2), HIFI_PLL,
                              metadata_.sysClockDivFactor - 1, metadata_.dClockDivFactor - 1,
                              TODDR_B, metadata_.version);
  if (lib_ == nullptr) {
    zxlogf(ERROR, "failed to create audio device");
    return ZX_ERR_NO_MEMORY;
  }

  InitHw();

  return ZX_OK;
}

void AudioStreamInDsp::InitHw() {
  // Enable first metadata_.number_of_channels channels.
  lib_->ConfigPdmIn(static_cast<uint8_t>((1 << metadata_.number_of_channels) - 1));
  lib_->SetRate(frames_per_second_);
  lib_->Sync();
}

zx_status_t AudioStreamInDsp::ChangeFormat(const audio_proto::StreamSetFmtReq& req) {
  driver_transfer_bytes_ = lib_->fifo_depth();
  external_delay_nsec_ = 0;

  if (req.channels != metadata_.number_of_channels) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (req.frames_per_second != 48'000 && req.frames_per_second != 96'000) {
    return ZX_ERR_INVALID_ARGS;
  }
  frames_per_second_ = req.frames_per_second;

  InitHw();

  return ZX_OK;
}

// At certain intervals, the offset of DMA pointer in the ring buffer is obtained, and the HW DSP FW
// is notified to adjust the position of the data processing pointer.
void AudioStreamInDsp::RingNotificationReport() {
  ScopedToken t(domain_token());
  position_timer_.PostDelayed(dispatcher(), kTransferInterval);
  uint32_t ring_buffer_pos = lib_->GetRingPosition();
  zx_status_t status = audio_mailbox_->DspProcessTaskPosition(ring_buffer_pos);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Notify DSP data process location information failed: %s",
           zx_status_get_string(status));
  }
}

zx_status_t AudioStreamInDsp::GetBuffer(const audio_proto::RingBufGetBufferReq& req,
                                        uint32_t* out_num_rb_frames, zx::vmo* out_buffer) {
  size_t ring_buffer_size = fbl::round_up<size_t, size_t>(
      req.min_ring_buffer_frames * frame_size_, std::lcm(frame_size_, lib_->GetBufferAlignment()));
  size_t out_frames = ring_buffer_size / frame_size_;
  if (out_frames > std::numeric_limits<uint32_t>::max()) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Make sure the DMA is stopped before releasing quarantine.
  lib_->Stop();
  // Make sure that all reads/writes have gone through.
  BarrierBeforeRelease();

  size_t vmo_size = fbl::round_up<size_t, size_t>(ring_buffer_size, zx_system_get_page_size());
  uint32_t transfer_length =
      frame_size_ * frames_per_second_ * static_cast<uint32_t>(kTransferInterval.to_msecs()) / 1000;
  AddrInfo addr_info = {.addr_length = vmo_size,
                        .buff_size = ring_buffer_size,
                        .is_output = 0,
                        .pid = static_cast<uint32_t>(getpid()),
                        .interval = static_cast<uint32_t>(kTransferInterval.to_msecs()),
                        .length = transfer_length};

  zx_status_t status = audio_mailbox_->DspCreateProcessingTask(&addr_info, sizeof(addr_info));
  if (status != ZX_OK) {
    return status;
  }

  const uint32_t dst_addr = static_cast<uint32_t>(addr_info.dst_addr);
  const uint32_t src_addr = static_cast<uint32_t>(addr_info.src_addr);

  zx_paddr_t mmio_vmo_paddr;
  zx::pmt pmt;
  status = bti_.pin(ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, *zx::unowned_vmo(dsp_mmio_->get_vmo()), 0,
                    ZX_PAGE_SIZE, &mmio_vmo_paddr, 1, &pmt);
  if (status != ZX_OK) {
    zxlogf(ERROR, "unable to pin memory: %s", zx_status_get_string(status));
    return status;
  }

  status = pmt.unpin();
  ZX_DEBUG_ASSERT(status == ZX_OK);

  // HW DSP shares SRAM with ARM for data transmission.
  uint32_t sram_addr = static_cast<uint32_t>(mmio_vmo_paddr);
  ZX_DEBUG_ASSERT(sram_addr <= src_addr);
  ZX_DEBUG_ASSERT(src_addr % zx_system_get_page_size() == 0);
  zx_off_t shared_sram_offset = src_addr - sram_addr;
  ZX_DEBUG_ASSERT(shared_sram_offset + vmo_size <= dsp_mmio_->get_size());
  status = dsp_mmio_->get_vmo()->create_child(ZX_VMO_CHILD_SLICE, shared_sram_offset, vmo_size,
                                              out_buffer);
  if (status != ZX_OK) {
    zxlogf(ERROR, "src_mmio_ create child vmo failed: %s", zx_status_get_string(status));
    return status;
  }

  status = lib_->SetBuffer(dst_addr, ring_buffer_size);
  if (status != ZX_OK) {
    zxlogf(ERROR, "failed to set buffer: %s", zx_status_get_string(status));
    return status;
  }

  // This is safe because of the overflow check we made above.
  *out_num_rb_frames = static_cast<uint32_t>(out_frames);
  ring_buffer_size_ = ring_buffer_size;
  return status;
}

void AudioStreamInDsp::RingBufferShutdown() { lib_->Shutdown(); }

zx_status_t AudioStreamInDsp::Start(uint64_t* out_start_time) {
  *out_start_time = lib_->Start();

  uint32_t notifs = LoadNotificationsPerRing();
  if (notifs) {
    ZX_DEBUG_ASSERT(ring_buffer_size_ > 0);
    notification_rate_ = zx::duration(zx_duration_from_usec(
        1'000 * ring_buffer_size_ / (frame_size_ * frames_per_second_ / 1'000 * notifs)));
    notify_timer_.PostDelayed(dispatcher(), notification_rate_);
  } else {
    notification_rate_ = {};
  }

  /* During recording, after TODDR writes data to the specified address, data processing starts in
   * HW DSP FW. */
  zx_status_t status = audio_mailbox_->DspProcessTaskStart();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Dsp data process start failed: %s", zx_status_get_string(status));
    return status;
  }
  position_timer_.PostDelayed(dispatcher(), kTransferInterval);
  return ZX_OK;
}

// Timer handler for sending out position notifications.
void AudioStreamInDsp::ProcessRingNotification() {
  ScopedToken t(domain_token());
  ZX_ASSERT(notification_rate_ != zx::duration());

  notify_timer_.PostDelayed(dispatcher(), notification_rate_);

  audio_proto::RingBufPositionNotify resp = {};
  resp.hdr.cmd = AUDIO_RB_POSITION_NOTIFY;

  resp.monotonic_time = zx::clock::get_monotonic().get();
  resp.ring_buffer_pos = lib_->GetRingPosition();
  NotifyPosition(resp);
}

void AudioStreamInDsp::ShutdownHook() {
  if (running_.load()) {
    running_.store(false);
    irq_.destroy();
    thrd_join(thread_, NULL);
  }
  lib_->Shutdown();
}

zx_status_t AudioStreamInDsp::Stop() {
  notify_timer_.Cancel();
  notification_rate_ = {};
  lib_->Stop();

  zx_status_t status = audio_mailbox_->DspProcessTaskStop();
  if (status != ZX_OK) {
    return status;
  }
  position_timer_.Cancel();
  return ZX_OK;
}

zx_status_t AudioStreamInDsp::AddFormats() {
  fbl::AllocChecker ac;
  supported_formats_.reserve(1, &ac);
  if (!ac.check()) {
    zxlogf(ERROR, "Out of memory, can not create supported formats list");
    return ZX_ERR_NO_MEMORY;
  }

  SimpleAudioStream::SupportedFormat format = {};
  format.range.min_channels = metadata_.number_of_channels;
  format.range.max_channels = metadata_.number_of_channels;
  format.range.sample_formats = AUDIO_SAMPLE_FORMAT_16BIT;
  format.range.min_frames_per_second = kMinSampleRate;
  format.range.max_frames_per_second = kMaxSampleRate;
  format.range.flags = ASF_RANGE_FLAG_FPS_48000_FAMILY;

  supported_formats_.push_back(std::move(format));

  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = AudioStreamInDsp::Create;
  return ops;
}();

}  // namespace audio::aml_g12

ZIRCON_DRIVER(aml_g12_pdm, audio::aml_g12::driver_ops, "zircon", "0.1");
