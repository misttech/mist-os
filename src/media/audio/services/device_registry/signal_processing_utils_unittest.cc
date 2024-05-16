// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/signal_processing_utils.h"

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/common_types.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/signal_processing_utils_unittest.h"

namespace media_audio {
namespace {

// These cases unittest the Map... functions with inputs that cause INFO logging (if any).

TEST(SignalProcessingUtilsTest, MapElements) {
  auto map = MapElements(kElements);
  EXPECT_EQ(map.size(), kElements.size());

  EXPECT_EQ(*map.at(*kDaiInterconnectElement.id()).element.type(), *kDaiInterconnectElement.type());
  EXPECT_TRUE(map.at(*kDaiInterconnectElement.id()).element.can_stop().value_or(false));

  EXPECT_EQ(*map.at(*kRingBufferElement.id()).element.type(), *kRingBufferElement.type());

  EXPECT_EQ(*map.at(*kAgcElement.id()).element.type(), *kAgcElement.type());
  EXPECT_FALSE(map.at(*kAgcElement.id()).element.can_stop().value_or(true));
  EXPECT_EQ(map.at(*kAgcElement.id()).element.description()->at(255), 'X');

  EXPECT_EQ(*map.at(*kDynamicsElement.id()).element.type(), *kDynamicsElement.type());
  EXPECT_TRUE(map.at(*kDynamicsElement.id()).element.can_bypass().value_or(false));
}

TEST(SignalProcessingUtilsTest, MapTopologies) {
  auto map = MapTopologies(kTopologies);
  EXPECT_EQ(map.size(), 3u);

  EXPECT_EQ(map.at(kTopologyDaiAgcDynRbId).size(), 3u);
  EXPECT_EQ(map.at(kTopologyDaiAgcDynRbId).at(0).processing_element_id_from(),
            kDaiInterconnectElementId);
  EXPECT_EQ(map.at(kTopologyDaiAgcDynRbId).at(0).processing_element_id_to(), kAgcElementId);

  EXPECT_EQ(map.at(kTopologyDaiRbId).size(), 1u);
  EXPECT_EQ(map.at(kTopologyDaiRbId).front().processing_element_id_from(),
            kDaiInterconnectElementId);
  EXPECT_EQ(map.at(kTopologyDaiRbId).front().processing_element_id_to(), kRingBufferElementId);

  EXPECT_EQ(map.at(kTopologyRbDaiId).size(), 1u);
  EXPECT_EQ(map.at(kTopologyRbDaiId).front().processing_element_id_from(), kRingBufferElementId);
  EXPECT_EQ(map.at(kTopologyRbDaiId).front().processing_element_id_to(), kDaiInterconnectElementId);
}

}  // namespace
}  // namespace media_audio
