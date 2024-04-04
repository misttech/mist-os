// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/signal_processing_utils.h"

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/signal_processing_utils_unittest.h"

namespace media_audio {

// These cases unittest the Map... functions with inputs that cause INFO logging (if any).

TEST(SignalProcessingUtilsTest, MapElements) {
  auto map = MapElements(kElements);
  EXPECT_EQ(map.size(), kElements.size());

  EXPECT_EQ(*map.at(*kElement1.id()).element.type(), *kElement1.type());
  EXPECT_EQ(*map.at(*kElement1.id()).element.type_specific()->endpoint()->type(),
            fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect);

  EXPECT_EQ(*map.at(*kElement2.id()).element.type(), *kElement2.type());

  EXPECT_EQ(*map.at(*kElement3.id()).element.type(), *kElement3.type());
  EXPECT_TRUE(map.at(*kElement3.id()).element.can_disable().value_or(false));
  EXPECT_EQ(map.at(*kElement3.id()).element.description()->at(255), 'X');

  EXPECT_EQ(*map.at(*kElement4.id()).element.type(), *kElement4.type());
  EXPECT_EQ(*map.at(*kElement4.id()).element.type_specific()->endpoint()->type(),
            fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer);
}

TEST(SignalProcessingUtilsTest, MapTopologies) {
  auto map = MapTopologies(kTopologies);
  EXPECT_EQ(map.size(), 3u);

  EXPECT_EQ(map.at(kTopologyId1234).size(), 3u);
  EXPECT_EQ(map.at(kTopologyId1234).at(0).processing_element_id_from(), kElementId1);
  EXPECT_EQ(map.at(kTopologyId1234).at(0).processing_element_id_to(), kElementId2);

  EXPECT_EQ(map.at(kTopologyId14).size(), 1u);
  EXPECT_EQ(map.at(kTopologyId14).front().processing_element_id_from(), kElementId1);
  EXPECT_EQ(map.at(kTopologyId14).front().processing_element_id_to(), kElementId4);

  EXPECT_EQ(map.at(kTopologyId41).size(), 1u);
  EXPECT_EQ(map.at(kTopologyId41).front().processing_element_id_from(), kElementId4);
  EXPECT_EQ(map.at(kTopologyId41).front().processing_element_id_to(), kElementId1);
}

}  // namespace media_audio
