// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// This file contains functions that post comments on gerrit changes for RFCs.

const LAST_CALL_COMMENT = 'This RFC is now in Last Call! Reviewers, please \
post any remaining comments in the next 7 days. Note that the RFC will only \
be accepted once all the comment threads have come to conclusion.';

// Post `message` as a top-level comment to the current revision of `changeId`.
function _postComment(changeId, message) {
  const accessToken = ScriptApp.getOAuthToken();

  const payload = { message };

  var options = {
    'method': 'post',
    'contentType': 'application/json',
    'payload': JSON.stringify(payload)
  };

  const response = UrlFetchApp.fetch(GERRIT_API_URL
    + `/a/changes/${changeId}/revisions/current/review?access_token=`
    + accessToken,
    options);

  console.log("Post comment response: " + response.getResponseCode());
}

// Post a top-level comment to the current revision of `changeId`, telling
// everyone that the RFC is now in last call.
function postLastCallComment(changeId) {
  _postComment(changeId, LAST_CALL_COMMENT);
}
