// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/watcher.h>
#include <lib/fhcp/cpp/fhcp.h>
#include <lib/fit/function.h>
#include <lib/sync/cpp/completion.h>

#include <gtest/gtest.h>

#include "src/lib/fsl/io/device_watcher.h"

namespace fir = fuchsia_input_report;

constexpr zx::duration kPerQuadrantTimeout = zx::sec(30);

struct Midpoints {
  int64_t x_midpoint;
  int64_t y_midpoint;
};

enum class Quadrant { kTopLeft, kTopRight, kBottomLeft, kBottomRight };

void ConfigureTouchEvents(fidl::WireSyncClient<fir::InputDevice>& client);
void ConnectToTouchpad(fidl::WireSyncClient<fir::InputDevice>* out_client,
                       Midpoints* out_midpoints);
void WaitForTouchAndRelease(fidl::WireSyncClient<fir::InputReportsReader>& client,
                            Midpoints midpoints, Quadrant desired_quadrant);
zx_status_t RunWithTimeout(zx::duration timeout, fit::closure f);

TEST(TouchpadTests, AreaCoverage) {
  // This test verifies that the touchpad driver can report touches at all four corners of the
  // touchpad.

  fidl::WireSyncClient<fir::InputDevice> input_device_client;
  Midpoints midpoints;
  ConnectToTouchpad(&input_device_client, &midpoints);
  ASSERT_TRUE(input_device_client.is_valid());
  ConfigureTouchEvents(input_device_client);

  // Get the InputReportsReader client from the InputDevice protocol.
  auto endpoints = fidl::CreateEndpoints<fir::InputReportsReader>();
  ASSERT_TRUE(endpoints.is_ok());
  ASSERT_EQ(ZX_OK,
            input_device_client->GetInputReportsReader(std::move(endpoints->server)).status());

  auto reader_client = fidl::WireSyncClient<fir::InputReportsReader>(std::move(endpoints->client));

  // The test itself - check for touches in each corner.
  //
  // We specify a time-out in the code itself rather than in the build rule to ensure that it is
  // honored in all contexts that the test is run. For instance, `ffx test` does not use the
  // time-out in the build file, nor does `ffx driver conformance`.
  ASSERT_EQ(ZX_OK, RunWithTimeout(kPerQuadrantTimeout, [&reader_client, midpoints]() {
              WaitForTouchAndRelease(reader_client, midpoints, Quadrant::kTopLeft);
            }));
  ASSERT_EQ(ZX_OK, RunWithTimeout(kPerQuadrantTimeout, [&reader_client, midpoints]() {
              WaitForTouchAndRelease(reader_client, midpoints, Quadrant::kTopRight);
            }));
  ASSERT_EQ(ZX_OK, RunWithTimeout(kPerQuadrantTimeout, [&reader_client, midpoints]() {
              WaitForTouchAndRelease(reader_client, midpoints, Quadrant::kBottomRight);
            }));
  ASSERT_EQ(ZX_OK, RunWithTimeout(kPerQuadrantTimeout, [&reader_client, midpoints]() {
              WaitForTouchAndRelease(reader_client, midpoints, Quadrant::kBottomLeft);
            }));
}

Quadrant GetQuadrant(const fir::wire::ContactInputReport& contact, Midpoints midpoints) {
  bool in_left_half = contact.position_x() < midpoints.x_midpoint;
  bool in_top_half = contact.position_y() < midpoints.y_midpoint;

  if (in_left_half) {
    return in_top_half ? Quadrant::kTopLeft : Quadrant::kBottomLeft;
  } else {
    return in_top_half ? Quadrant::kTopRight : Quadrant::kBottomRight;
  }
}

const char* GetQuadrantName(Quadrant q) {
  switch (q) {
    case Quadrant::kTopLeft:
      return "top left";
    case Quadrant::kTopRight:
      return "top right";
    case Quadrant::kBottomLeft:
      return "bottom left";
    case Quadrant::kBottomRight:
      return "bottom right";
  }
}

void WaitForRelease(fidl::WireSyncClient<fir::InputReportsReader>& client) {
  // Wait for the touch to be released (indicated by an empty contacts vector).
  while (true) {
    auto result = client->ReadInputReports();
    ASSERT_EQ(ZX_OK, result.status());
    for (fir::wire::InputReport& report : result->value()->reports) {
      ASSERT_TRUE(report.has_touch());
      if (report.touch().contacts().empty()) {
        fhcp::PrintManualTestingMessage("Release detected.");
        return;
      }
    }
  }
}

void WaitForTouch(fidl::WireSyncClient<fir::InputReportsReader>& client, Midpoints midpoints,
                  Quadrant desired_quadrant) {
  auto result = client->ReadInputReports();
  ASSERT_EQ(ZX_OK, result.status());

  ASSERT_FALSE(result->value()->reports.empty()) << "No input reports received.";

  // Wait for a touch event. We ensure that all reports in the FIDL response contain valid touch
  // reports in the expected quadrant.
  for (fir::wire::InputReport& report : result->value()->reports) {
    ASSERT_TRUE(report.has_touch());
    ASSERT_FALSE(report.touch().contacts().empty());
    const fir::wire::ContactInputReport& contact_report = report.touch().contacts()[0];
    Quadrant quadrant = GetQuadrant(contact_report, midpoints);
    ASSERT_EQ(quadrant, desired_quadrant)
        << "Touch expected in the " << GetQuadrantName(desired_quadrant) << " but detected in the "
        << GetQuadrantName(quadrant);
  }
}

void WaitForTouchAndRelease(fidl::WireSyncClient<fir::InputReportsReader>& client,
                            Midpoints midpoints, Quadrant desired_quadrant) {
  fhcp::PrintManualTestingMessage("\n\n*** Please touch the %s corner of the touchpad and hold.",
                                  GetQuadrantName(desired_quadrant));
  fhcp::PrintManualTestingMessage("\nThe test will automatically time out after %ld seconds.",
                                  kPerQuadrantTimeout.to_secs());

  WaitForTouch(client, midpoints, desired_quadrant);

  fhcp::PrintManualTestingMessage("Please release finger.");
  WaitForRelease(client);
}

void ConnectToTouchpad(fidl::WireSyncClient<fir::InputDevice>* out_client,
                       Midpoints* out_midpoints) {
  // Output parameters are used because ASSERT_* macros only work in functions that return void.

  // Iterate over the devices in /dev/class/input-report/ looking for the one that corresponds to
  // the touchpad.
  constexpr char kInputReportDevPath[] = "/dev/class/input-report";
  // TODO(https://fxbug.dev/113882): Use a nicer wrapper for this pattern when
  // it exists.
  fbl::unique_fd fd;
  ASSERT_EQ(fdio_open_fd(kInputReportDevPath, /*flags=*/0, fd.reset_and_get_address()), ZX_OK);
  std::pair out = std::make_pair(out_client, out_midpoints);
  ASSERT_EQ(fdio_watch_directory(
                fd.get(),
                [](int dirfd, int event, const char* fn, void* cookie) {
                  if (std::string_view{fn} == ".") {
                    return ZX_OK;
                  }
                  if (event != WATCH_EVENT_ADD_FILE) {
                    return ZX_OK;
                  }
                  fdio_cpp::UnownedFdioCaller caller(dirfd);
                  zx::result client_end =
                      component::ConnectAt<fir::InputDevice>(caller.directory(), fn);
                  if (client_end.is_error()) {
                    return client_end.error_value();
                  }
                  fidl::WireSyncClient input_device_client(std::move(client_end.value()));

                  // Get the device's descriptor and skip devices that aren't touchpads.
                  const fidl::WireResult descriptor_result = input_device_client->GetDescriptor();
                  if (!descriptor_result.ok()) {
                    return descriptor_result.status();
                  }
                  const fidl::WireResponse descriptor_response = descriptor_result.value();
                  if (!descriptor_response.descriptor.has_touch() ||
                      descriptor_response.descriptor.touch().input().touch_type() !=
                          fir::TouchType::kTouchpad) {
                    return ZX_OK;
                  }

                  // Need at least one contact entry to get the dimensions of the touchpad.
                  if (descriptor_response.descriptor.touch().input().contacts().empty()) {
                    return ZX_ERR_BAD_STATE;
                  }
                  const fir::wire::ContactInputDescriptor& contact =
                      descriptor_response.descriptor.touch().input().contacts()[0];
                  int64_t min_x = contact.position_x().range.min;
                  int64_t max_x = contact.position_x().range.max;
                  int64_t min_y = contact.position_y().range.min;
                  int64_t max_y = contact.position_y().range.max;
                  Midpoints midpoints{
                      .x_midpoint = (max_x + min_x) / 2,
                      .y_midpoint = (max_y + min_y) / 2,
                  };
                  auto [out_client, out_midpoints] = *reinterpret_cast<decltype(&out)>(cookie);
                  *out_client = std::move(input_device_client);
                  *out_midpoints = midpoints;

                  return ZX_ERR_STOP;
                },
                ZX_TIME_INFINITE, &out),
            ZX_ERR_STOP);
}

void ConfigureTouchEvents(fidl::WireSyncClient<fir::InputDevice>& client) {
  // By default the touchpad only reports mouse events, so we use SetFeatureReport to turn on
  // touch events, which allow us to detect when a finger is released.
  fidl::Arena allocator;
  auto touch_report = fir::wire::TouchFeatureReport::Builder(allocator);
  touch_report.input_mode(fir::TouchConfigurationInputMode::kWindowsPrecisionTouchpadCollection);
  auto feature_report = fir::wire::FeatureReport::Builder(allocator);
  feature_report.touch(touch_report.Build());
  ASSERT_EQ(ZX_OK, client->SetFeatureReport(feature_report.Build()).status());
}

zx_status_t RunWithTimeout(zx::duration timeout, fit::closure f) {
  libsync::Completion completion;
  std::thread thrd = std::thread([&f, &completion]() {
    f();
    completion.Signal();
  });

  zx_status_t status = completion.Wait(timeout);
  if (status == ZX_OK) {
    thrd.join();
  } else {
    // Detach the thread to be killed when the process exits. We can't join here because that would
    // defeat the purpose of the time-out.
    thrd.detach();
    if (status == ZX_ERR_TIMED_OUT) {
      ADD_FAILURE() << "Test timed out after " << kPerQuadrantTimeout.to_secs() << " seconds.";
    }
  }
  return status;
}
