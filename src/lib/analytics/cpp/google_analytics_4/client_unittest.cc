// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/analytics/cpp/google_analytics_4/client.h"

#include <memory>
#include <ostream>
#include <string>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/lib/analytics/cpp/google_analytics_4/event.h"
#include "src/lib/fxl/strings/substitute.h"
#include "third_party/rapidjson/include/rapidjson/document.h"
#include "third_party/rapidjson/include/rapidjson/prettywriter.h"
#include "third_party/rapidjson/include/rapidjson/stringbuffer.h"

namespace rapidjson {

std::ostream& operator<<(std::ostream& os, const Document& doc) {
  StringBuffer s;
  PrettyWriter<StringBuffer> writer(s);
  writer.SetIndent(' ', 2);
  doc.Accept(writer);
  return os << s.GetString();
}

}  // namespace rapidjson

namespace analytics::google_analytics_4 {

class MockClient : public Client {
 public:
  void expectEqBody(const std::string& body) {
    rapidjson::Document other, self;
    other.Parse(body);
    self.Parse(body_);
    EXPECT_EQ(self, other);
  }

 private:
  void SendData(std::string body) override { body_ = std::move(body); }

  std::string body_;
};

class MockEvent : public Event {
 public:
  using Event::SetParameter;
  MockEvent(std::string name) : Event(std::move(name)) {}
};

class ClientTest : public ::testing::Test {
 protected:
  void SetUp() override {
    client_.SetQueryParameters("IGNORED_IN_TEST_BUT_NEEDED", "IGNORED_IN_TEST_BUT_NEEDED");
    client_.SetClientId("TEST-CLIENT");
  }

  MockClient client_;
};

TEST_F(ClientTest, SimpleEvent) {
  auto event = std::make_unique<MockEvent>("test-event");
  auto body = fxl::Substitute(
      R"JSON(
      {
        "client_id": "TEST-CLIENT",
        "events": [
          {
            "name": "test-event",
            "timestamp_micros": $0
          }
        ]
      }
  )JSON",
      std::to_string(event->timestamp_micros().count()));
  client_.AddEvent(std::move(event));
  client_.expectEqBody(body);
}

TEST_F(ClientTest, SimpleEventWithUserProperty) {
  client_.SetUserProperty("test-key", "test-val");
  auto event = std::make_unique<MockEvent>("test-event");
  auto body = fxl::Substitute(
      R"JSON(
      {
        "client_id": "TEST-CLIENT",
        "events": [
          {
            "name": "test-event",
            "timestamp_micros": $0
          }
        ],
        "user_properties": {"test-key": {"value": "test-val"}}
      }
  )JSON",
      std::to_string(event->timestamp_micros().count()));
  client_.AddEvent(std::move(event));
  client_.expectEqBody(body);
}

TEST_F(ClientTest, SimpleEventWithUserProperties) {
  client_.SetUserProperty("test-key-1", "test-val-1");
  client_.SetUserProperty("test-key-2", 2);
  client_.SetUserProperty("test-key-3", false);
  client_.SetUserProperty("test-key-4", 2.5);
  auto event = std::make_unique<MockEvent>("test-event");
  auto body = fxl::Substitute(
      R"JSON(
      {
        "client_id": "TEST-CLIENT",
        "events": [
          {
            "name": "test-event",
            "timestamp_micros": $0
          }
        ],
        "user_properties": {
          "test-key-1": {"value": "test-val-1"},
          "test-key-2": {"value": 2},
          "test-key-3": {"value": false},
          "test-key-4": {"value": 2.5}
        }
      }
    )JSON",
      std::to_string(event->timestamp_micros().count()));
  client_.AddEvent(std::move(event));
  client_.expectEqBody(body);
}

TEST_F(ClientTest, ConsecutiveEventsWithUserProperties) {
  client_.SetUserProperty("test-key-1", "test-val-1");
  client_.SetUserProperty("test-key-2", "test-val-2");
  auto event1 = std::make_unique<MockEvent>("test-event-1");
  auto body1 = fxl::Substitute(
      R"JSON(
      {
        "client_id": "TEST-CLIENT",
        "events": [
          {
            "name": "test-event-1",
            "timestamp_micros": $0
          }
        ],
        "user_properties": {
          "test-key-1": {"value": "test-val-1"},
          "test-key-2": {"value": "test-val-2"}
        }
      }
  )JSON",
      std::to_string(event1->timestamp_micros().count()));
  client_.AddEvent(std::move(event1));
  client_.expectEqBody(body1);

  auto event2 = std::make_unique<MockEvent>("test-event-2");
  auto body2 = fxl::Substitute(
      R"JSON(
      {
        "client_id": "TEST-CLIENT",
        "events": [
          {
            "name": "test-event-2",
            "timestamp_micros": $0
          }
        ],
        "user_properties": {
          "test-key-1": {"value": "test-val-1"},
          "test-key-2": {"value": "test-val-2"}
        }
      }
  )JSON",
      std::to_string(event2->timestamp_micros().count()));
  client_.AddEvent(std::move(event2));
  client_.expectEqBody(body2);
}

TEST_F(ClientTest, EventWithParameters) {
  auto event = std::make_unique<MockEvent>("test-event");
  event->SetParameter("test-key-1", "test-val-1");
  event->SetParameter("test-key-2", 2);
  event->SetParameter("test-key-3", false);
  event->SetParameter("test-key-4", 2.5);
  auto body = fxl::Substitute(
      R"JSON(
      {
        "client_id": "TEST-CLIENT",
        "events": [
          {
            "name": "test-event",
            "params": {
              "test-key-1": "test-val-1",
              "test-key-2": 2,
              "test-key-3": false,
              "test-key-4": 2.5
            },
            "timestamp_micros": $0
          }
        ]
      }
  )JSON",
      std::to_string(event->timestamp_micros().count()));
  client_.AddEvent(std::move(event));
  client_.expectEqBody(body);
}

}  // namespace analytics::google_analytics_4
