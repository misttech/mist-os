// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.dotmatrixdisplay/cpp/wire.h>
#include <fidl/fuchsia.hardware.ftdi/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <filesystem>

constexpr uint32_t kWidth = 128;
constexpr uint32_t kHeight = 64;

std::vector<uint8_t> frame_buffer_(static_cast<size_t>(kWidth) * 8);

void ClearScreen() {
  for (uint32_t i = 0; i < 8; i++) {
    for (uint32_t j = 0; j < kWidth; j++) {
      frame_buffer_[i * kWidth + j] = 0;
    }
  }
}

void SetPixel(uint32_t x, uint32_t y) {
  uint32_t row = y / 8;
  uint32_t offset = y % 8;

  uint8_t data = frame_buffer_[row * kWidth + x];
  uint8_t mask = static_cast<uint8_t>(1 << offset);
  frame_buffer_[row * kWidth + x] = static_cast<uint8_t>(data | mask);
}

class Invader {
 public:
  static constexpr uint32_t kXSize = 11;
  static constexpr uint32_t kYSize = 7;

  Invader(uint32_t x, uint32_t y) {
    x_ = x;
    y_ = y;
  }

  void Update(uint32_t rel_x, uint32_t rel_y) {
    x_ += rel_x;
    y_ += rel_y;
  }

  void Draw() const {
    uint32_t x_vals[] = {
        3, 9, 4, 8, 3,  4,  5, 6, 7, 8, 9, 2, 3, 5, 6,  7, 9, 10, 1,  2, 3, 4, 5,
        6, 7, 8, 9, 10, 11, 1, 3, 4, 5, 6, 7, 8, 9, 11, 1, 3, 9,  11, 4, 5, 7, 8,
    };
    uint32_t y_vals[] = {
        0, 0, 1, 1, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7,
    };
    static_assert(std::size(x_vals) == std::size(y_vals));
    for (uint32_t i = 0; i < std::size(x_vals); i++) {
      SetPixel(x_ + x_vals[i], y_ + y_vals[i]);
    }
  }

 private:
  uint32_t x_ = 0;
  uint32_t y_ = 0;
};

class InvaderBlock {
 public:
  static constexpr uint32_t kBlockWidth = kWidth - 30;
  static constexpr uint32_t kHeightJump = 3;
  static constexpr uint32_t kNumRows = 3;
  static constexpr uint32_t kBlockHeight = (Invader::kYSize + 3) * kNumRows;

  InvaderBlock() {
    for (uint32_t rows = 0; rows < 3; rows++) {
      for (uint32_t i = 0; i < kBlockWidth / Invader::kXSize; i++) {
        invaders_.emplace_back(i * (Invader::kXSize + 1), rows * (Invader::kYSize + 3));
      }
    }
  }

  void UpdateAndDraw() {
    uint32_t rel_x = 0;
    uint32_t rel_y = 0;
    if (!IsTurnAround()) {
      rel_x = x_jump_;
    } else {
      rel_y += kHeightJump;
      x_jump_ *= -1;
      if (y_ + kBlockHeight + 5 >= kHeight) {
        rel_x = -1 * x_;
        rel_y = -1 * y_;
      }
    }
    x_ += rel_x;
    y_ += rel_y;
    for (Invader& i : invaders_) {
      i.Update(rel_x, rel_y);
      i.Draw();
    }
  }

 private:
  bool IsTurnAround() const {
    if (x_jump_ > 0) {
      return (x_ + kBlockWidth) >= kWidth;
    }
    return (x_ <= 0);
  }

  uint32_t x_ = 0;
  uint32_t y_ = 0;
  uint32_t x_jump_ = 1;
  std::vector<Invader> invaders_;
};

class Player {
 public:
  static constexpr int kBlockWidth = 8;
  void Draw() const {
    int x_vals[] = {
        4, 3, 5, 2, 6, 1, 7, 0, 8,
    };
    int y_vals[] = {
        0, 1, 1, 2, 2, 3, 3, 4, 4,
    };
    static_assert(std::size(x_vals) == std::size(y_vals));
    for (uint32_t i = 0; i < std::size(x_vals); i++) {
      SetPixel(x_ + x_vals[i], y_ + y_vals[i]);
    }
  }

  void UpdateAndDraw() {
    x_ += x_jump_;
    int rand = std::rand() % 20;
    if (rand == 0) {
      x_jump_ *= -1;
    }
    if (IsTurnAround()) {
      x_jump_ *= -1;
    }
    Draw();
  }

 private:
  bool IsTurnAround() const {
    if (x_jump_ > 0) {
      return (x_ + kBlockWidth) >= kWidth;
    }
    return (x_ <= 0);
  }
  uint32_t x_jump_ = 1;
  uint32_t x_ = 0;
  uint32_t y_ = kHeight - 5;
};

int RunInvaders() {
  constexpr char path[] = "/dev/class/dotmatrix-display/";
  for (const auto& entry : std::filesystem::directory_iterator(path)) {
    zx::result display = component::Connect<fuchsia_hardware_dotmatrixdisplay::DotmatrixDisplay>(
        entry.path().c_str());
    if (display.is_error()) {
      printf("connecting to %s failed: %s\n", entry.path().c_str(), display.status_string());
      return 1;
    }
    const fidl::WireResult result = fidl::WireCall(display.value())->GetConfig();
    if (!result.ok()) {
      printf("GetConfig on %s failed: %s\n", entry.path().c_str(),
             result.FormatDescription().c_str());
      return 1;
    }
    const fidl::WireResponse response = result.value();
    const fuchsia_hardware_dotmatrixdisplay::wire::DotmatrixDisplayConfig& display_config =
        response.config;
    if (display_config.width != kWidth || display_config.height != kHeight ||
        display_config.format !=
            fuchsia_hardware_dotmatrixdisplay::wire::PixelFormat::kMonochrome ||
        display_config.layout !=
            fuchsia_hardware_dotmatrixdisplay::wire::ScreenLayout::kColumnTbRowLr) {
      printf("Error: Display configs do not match supported config\n");
      printf("Width:  Support: %d Recieved: %d \n", kWidth, display_config.width);
      printf("Height: Support: %d Recieved: %d \n", kHeight, display_config.height);
      printf("Format: Support: %d Recieved: %d \n",
             static_cast<int>(fuchsia_hardware_dotmatrixdisplay::wire::PixelFormat::kMonochrome),
             static_cast<int>(display_config.format));
      printf(
          "Layout: Support: %d Recieved: %d \n",
          static_cast<int>(fuchsia_hardware_dotmatrixdisplay::wire::ScreenLayout::kColumnTbRowLr),
          static_cast<int>(display_config.layout));
      return 1;
    }

    InvaderBlock invBlock;
    Player player;
    Invader inv = Invader(0, 0);

    while (true) {
      ClearScreen();
      invBlock.UpdateAndDraw();
      player.UpdateAndDraw();

      const fidl::WireResult result =
          fidl::WireCall(display.value())
              ->SetScreen(fidl::VectorView<uint8_t>::FromExternal(frame_buffer_));
      if (!result.ok()) {
        printf("SetScreen on %s failed: %s\n", entry.path().c_str(),
               result.FormatDescription().c_str());
        return 1;
      }
      const fidl::WireResponse response = result.value();
      if (zx_status_t status = response.s; status != ZX_OK) {
        printf("SetScreen on %s failed: %s\n", entry.path().c_str(), zx_status_get_string(status));
        return 1;
      }
    }
  }

  printf("%s contained no entries\n", path);
  return 1;
}
