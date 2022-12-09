// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_TESTS_INTEGRATION_GRAPHICS_TESTS_WEB_PIXEL_TESTS_HTML_CONFIG_H_
#define SRC_UI_TESTS_INTEGRATION_GRAPHICS_TESTS_WEB_PIXEL_TESTS_HTML_CONFIG_H_

// Contains HTML code used in web_runner_pixel_tests.
namespace integration_tests {

// Displays a web page with a red background color.
constexpr auto kStaticHtml = R"(
<html>
  <head>
    <title>Static test page</title>
    <style>
      body { background-color: #ff0000 }
    </style>
  </head>
  <body>
  </body>
</html>
)";

// Displays a web page with a magenta background color. The color of the web page changes to blue
// on a tap event.
constexpr auto kDynamicHtml = R"(
<html>
  <head>
    <title>Dynamic test</title>
  </head>
  <body>
    <script>
      document.body.style.backgroundColor='#ff00ff';
      document.body.onclick = function(event) {
      document.body.style.backgroundColor='#0000ff';
    };
    </script>
  </body>
</html>
)";

}  // namespace integration_tests

#endif  // SRC_UI_TESTS_INTEGRATION_GRAPHICS_TESTS_WEB_PIXEL_TESTS_HTML_CONFIG_H_
