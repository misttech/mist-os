// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/observer_server.h"

namespace media_audio {
namespace {

class ObserverServerWarningTest : public AudioDeviceRegistryServerTestBase,
                                  public fidl::AsyncEventHandler<fuchsia_audio_device::Observer> {};

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//  unhealthy before WatchGainState. Expect Observer/Control/RingBuffer to drop, Reg/WatchRemove.

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//  unhealthy before WatchPlugState. Expect Observer/Control/RingBuffer to drop, Reg/WatchRemove.

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//  unhealthy before GetReferenceClock. Expect Observer/Control/RingBuffer to drop, Reg/WatchRemove.

}  // namespace
}  // namespace media_audio
