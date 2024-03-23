// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/tests/utils/utils.h"

#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/id.h>

#include <cmath>
#include <cstdint>

#include "src/ui/testing/util/screenshot_helper.h"

namespace integration_tests {

using InputCommand = fuchsia::ui::input::Command;
using fuchsia::ui::composition::ScreenshotFormat;
using fuchsia::ui::input::PointerEvent;
using fuchsia::ui::input::PointerEventPhase;
using fuchsia::ui::input::PointerEventType;
using ui_testing::Screenshot;

namespace {
std::ostream& operator<<(std::ostream& os, const PointerEventPhase& value) {
  switch (value) {
    case PointerEventPhase::ADD:
      return os << "add";
    case PointerEventPhase::HOVER:
      return os << "hover";
    case PointerEventPhase::DOWN:
      return os << "down";
    case PointerEventPhase::MOVE:
      return os << "move";
    case PointerEventPhase::UP:
      return os << "up";
    case PointerEventPhase::REMOVE:
      return os << "remove";
    case PointerEventPhase::CANCEL:
      return os << "cancel";
    default:
      return os << "<invalid enum value: " << static_cast<uint32_t>(value) << ">";
  }
}

std::ostream& operator<<(std::ostream& os, const PointerEventType& value) {
  switch (value) {
    case PointerEventType::TOUCH:
      return os << "touch";
    case PointerEventType::STYLUS:
      return os << "stylus";
    case PointerEventType::INVERTED_STYLUS:
      return os << "inverted stylus";
    case PointerEventType::MOUSE:
      return os << "mouse";
    default:
      return os << "<invalid enum value: " << static_cast<uint32_t>(value) << ">";
  }
}

}  // namespace

constexpr uint64_t kBytesPerPixel = 4;

// Used to compare whether two values are nearly equal.
// 1000 times machine limits to account for scaling from [0,1] to viewing volume [0,1000].
constexpr float kEpsilon = std::numeric_limits<float>::epsilon() * 1000;

bool PointerMatches(const PointerEvent& event, uint32_t pointer_id, PointerEventPhase phase,
                    float x, float y, PointerEventType type, uint32_t buttons) {
  bool result = true;
  if (event.type != type) {
    FX_LOGS(ERROR) << "  Actual type: " << event.type;
    FX_LOGS(ERROR) << "Expected type: " << type;
    result = false;
  }
  if (event.buttons != buttons) {
    FX_LOGS(ERROR) << "  Actual buttons: " << event.buttons;
    FX_LOGS(ERROR) << "Expected buttons: " << buttons;
    result = false;
  }
  if (event.pointer_id != pointer_id) {
    FX_LOGS(ERROR) << "  Actual id: " << event.pointer_id;
    FX_LOGS(ERROR) << "Expected id: " << pointer_id;
    result = false;
  }
  if (event.phase != phase) {
    FX_LOGS(ERROR) << "  Actual phase: " << event.phase;
    FX_LOGS(ERROR) << "Expected phase: " << phase;
    result = false;
  }
  if (fabs(event.x - x) > kEpsilon) {
    FX_LOGS(ERROR) << "  Actual x: " << event.x;
    FX_LOGS(ERROR) << "Expected x: " << x;
    result = false;
  }
  if (fabs(event.y - y) > kEpsilon) {
    FX_LOGS(ERROR) << "  Actual y: " << event.y;
    FX_LOGS(ERROR) << "Expected y: " << y;
    result = false;
  }
  return result;
}

bool CmpFloatingValues(float num1, float num2) {
  auto diff = fabs(num1 - num2);
  return diff < kEpsilon;
}

zx_koid_t ExtractKoid(const zx::object_base& object) {
  zx_info_handle_basic_t info{};
  if (object.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr) != ZX_OK) {
    return ZX_KOID_INVALID;  // no info
  }

  return info.koid;
}

zx_koid_t ExtractKoid(const fuchsia::ui::views::ViewRef& view_ref) {
  return ExtractKoid(view_ref.reference);
}

Mat3 ArrayToMat3(std::array<float, 9> array) {
  Mat3 mat;
  for (size_t row = 0; row < mat.size(); row++) {
    for (size_t col = 0; col < mat[0].size(); col++) {
      mat[row][col] = array[mat.size() * row + col];
    }
  }
  return mat;
}

Vec3 operator*(const Mat3& mat, const Vec3& vec) {
  Vec3 result = {0, 0, 0};
  for (size_t col = 0; col < mat[0].size(); col++) {
    for (size_t row = 0; row < mat.size(); row++) {
      result[col] += mat[row][col] * vec[row];
    }
  }
  return result;
}

Vec3& operator/(Vec3& vec, float num) {
  for (size_t i = 0; i < vec.size(); i++) {
    vec[i] /= num;
  }
  return vec;
}

Vec4 angleAxis(float angle, const Vec3& vec) {
  Vec4 result;
  result[0] = vec[0] * sin(angle * 0.5f);
  result[1] = vec[1] * sin(angle * 0.5f);
  result[2] = vec[2] * sin(angle * 0.5f);
  result[3] = cos(angle * 0.5f);
  return result;
}

Screenshot TakeScreenshot(const fuchsia::ui::composition::ScreenshotSyncPtr& screenshotter,
                          uint64_t width, uint64_t height, ScreenshotFormat format,
                          int display_rotation) {
  fuchsia::ui::composition::ScreenshotTakeRequest request;
  request.set_format(format);
  fuchsia::ui::composition::ScreenshotTakeResponse response;

  auto status = screenshotter->Take(std::move(request), &response);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to take screenshot: " << zx_status_get_string(status);
  }

  if (format == ScreenshotFormat::PNG) {
    return Screenshot(response.vmo());
  }
  return Screenshot(response.vmo(), width, height, display_rotation);
}

Screenshot TakeFileScreenshot(const fuchsia::ui::composition::ScreenshotSyncPtr& screenshotter,
                              uint64_t width, uint64_t height, ScreenshotFormat format,
                              int display_rotation) {
  fuchsia::ui::composition::ScreenshotTakeFileRequest request;
  request.set_format(format);
  fuchsia::ui::composition::ScreenshotTakeFileResponse response;

  auto status = screenshotter->TakeFile(std::move(request), &response);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to take screenshot: " << zx_status_get_string(status);
  }

  // Read img data from file.
  auto file = response.mutable_file()->BindSync();
  zx::vmo vmo_from_file;
  const auto vmo_size = width * height * kBytesPerPixel;
  FX_CHECK(zx::vmo::create(vmo_size, 0, &vmo_from_file) == ZX_OK);
  uint64_t offset = 0;
  uint64_t read_response_size = fuchsia::io::MAX_BUF;
  do {
    fuchsia::io::Readable_Read_Result result;
    // Can only read `MAX_BUF` bytes at a time.
    file->Read(fuchsia::io::MAX_BUF, &result);
    FX_CHECK(result.is_response()) << zx_status_get_string(result.err());

    const auto& response_data = result.response().data;
    read_response_size = response_data.size();
    vmo_from_file.write(response_data.data(), offset, read_response_size);
    offset += read_response_size;
  } while (read_response_size == fuchsia::io::MAX_BUF);

  if (format == ScreenshotFormat::PNG) {
    return Screenshot(vmo_from_file);
  }
  return Screenshot(vmo_from_file, width, height, display_rotation);
}

}  // namespace integration_tests
