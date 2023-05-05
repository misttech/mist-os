// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_DRIVER_H_
#define SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_DRIVER_H_

#include "src/media/audio/drivers/virtual_audio/virtual_audio_device_impl.h"

namespace virtual_audio {

struct CurrentFormat {
  uint32_t frames_per_second;
  uint32_t sample_format;
  uint32_t num_channels;
  zx::duration external_delay;
};
struct CurrentGain {
  bool mute;
  bool agc;
  float gain_db;
};
struct CurrentBuffer {
  zx::vmo vmo;
  uint32_t num_frames;
  uint32_t notifications_per_ring;
};
struct CurrentPosition {
  zx::time monotonic_time;
  uint32_t ring_position;
};

class VirtualAudioDriver {
 public:
  using ErrorT = fuchsia_virtualaudio::Error;

  // Thread safety token.
  //
  // This token acts like a "no-op mutex", allowing compiler thread safety annotations
  // to be placed on code or data that should only be accessed by a particular thread.
  // Any code that acquires the token makes the claim that it is running on the (single)
  // correct thread, and hence it is safe to access the annotated data and execute the annotated
  // code.
  struct __TA_CAPABILITY("role") Token {};
  class __TA_SCOPED_CAPABILITY ScopedToken {
   public:
    explicit ScopedToken(const Token& token) __TA_ACQUIRE(token) {}
    ~ScopedToken() __TA_RELEASE() {}
  };

  virtual ~VirtualAudioDriver() = default;

  // Execute the given task on a dispatcher.
  // Typically callbacks should acquire domain_token() before calling any methods.
  virtual void PostToDispatcher(fit::closure task_to_post) {
    async::PostTask(dispatcher(), std::move(task_to_post));
  }

  // Dispatcher used by PostToDispatcher().
  virtual async_dispatcher_t* dispatcher() = 0;

  // Used to mark that the task is executed in a given dispatcher context.
  const Token& domain_token() const __TA_RETURN_CAPABILITY(domain_token_) { return domain_token_; }

  // Shutdown the stream and request that it be unbound from the device tree.
  virtual void ShutdownAndRemove() = 0;

  //
  // The following methods implement getters and setters for fuchsia.virtualaudio.Device.
  // Default to not supported.
  // TODO(fxbug.dev/126797): Add the ability to trigger dynamic delay changes.
  //
  virtual fit::result<ErrorT, CurrentFormat> GetFormatForVA() __TA_REQUIRES(domain_token()) {
    return fit::error(ErrorT::kNotSupported);
  }
  virtual fit::result<ErrorT, CurrentGain> GetGainForVA() __TA_REQUIRES(domain_token()) {
    return fit::error(ErrorT::kNotSupported);
  }
  virtual fit::result<ErrorT, CurrentBuffer> GetBufferForVA() __TA_REQUIRES(domain_token()) {
    return fit::error(ErrorT::kNotSupported);
  }
  virtual fit::result<ErrorT, CurrentPosition> GetPositionForVA() __TA_REQUIRES(domain_token()) {
    return fit::error(ErrorT::kNotSupported);
  }
  virtual fit::result<ErrorT> SetNotificationFrequencyFromVA(uint32_t notifications_per_ring)
      __TA_REQUIRES(domain_token()) {
    return fit::error(ErrorT::kNotSupported);
  }
  virtual fit::result<ErrorT> ChangePlugStateFromVA(bool plugged) __TA_REQUIRES(domain_token()) {
    return fit::error(ErrorT::kNotSupported);
  }
  virtual fit::result<ErrorT> AdjustClockRateFromVA(int32_t ppm_from_monotonic)
      __TA_REQUIRES(domain_token()) {
    return fit::error(ErrorT::kNotSupported);
  }

 private:
  Token domain_token_;
};

}  // namespace virtual_audio

#endif  // SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_DRIVER_H_
