// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <zircon/errors.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/signal_processing_utils.h"
#include "src/media/audio/services/device_registry/validate_unittest.h"

// #include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
// #include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
// #include <lib/zx/clock.h>
// #include <lib/zx/time.h>
// #include <cmath>
// #include <cstdint>
// #include <optional>
// #include <set>
// #include <vector>

namespace media_audio {

// signalprocessing functions
//
TEST(SignalProcessingUtilsWarningTest, BadElementList) {
  EXPECT_TRUE(MapElements(kEmptyElements).empty());

  // List contains two elements with the same id.
  EXPECT_TRUE(MapElements(kElementsDuplicateId).empty());

  // bad Elements: all the ValidateElement negative cases
  EXPECT_TRUE(MapElements(kElementsWithNoId).empty());
  EXPECT_TRUE(MapElements(kElementsWithNoType).empty());
  EXPECT_TRUE(MapElements(kElementsWithNoRequiredTypeSpecific).empty());
  EXPECT_TRUE(MapElements(kElementsWithWrongTypeSpecific).empty());
  EXPECT_TRUE(MapElements(kElementsWithEmptyDescription).empty());
}

TEST(SignalProcessingUtilsWarningTest, BadTopologyList) {
  EXPECT_TRUE(MapTopologies(kEmptyTopologies).empty());

  // List contains two topologies with the same id.
  EXPECT_TRUE(MapTopologies(kTopologiesWithDuplicateId).empty());

  // Topology list with a bad Topology: all the ValidateTopology negative cases
  EXPECT_TRUE(MapTopologies(kTopologiesWithMissingId).empty());
  EXPECT_TRUE(MapTopologies(kTopologiesWithMissingEdgePairs).empty());
  EXPECT_TRUE(MapTopologies(kTopologiesWithEmptyEdgePairs).empty());
}

}  // namespace media_audio
