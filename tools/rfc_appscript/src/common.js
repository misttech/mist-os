// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// This file contains constants and functions to be used by other files in this
// directory.

const GERRIT_API_URL = 'https://fuchsia-review.googlesource.com';

// Takes a value returned by UrlFetchApp.fetch() and logs and returns a parsed
// JSON object with the result.
//
// See
// https://gerrit-review.googlesource.com/Documentation/rest-api-changes.html
// for documentation.
function parseGerritResponse(response) {
  const rawJson = response.getContentText().substring(5);
  console.log('Gerrit Response: ', rawJson);
  return JSON.parse(rawJson);
}
