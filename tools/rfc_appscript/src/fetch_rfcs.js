// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// This file contains javascript which gets deployed to scripts.google.com, in
// order to power custom logic for the internal RFC tracker.

const TABLE_ID = 'bbfvcDiuyZ08S5ZJ7vN4ZI';
const TABLE_NAME = 'tables/' + TABLE_ID;
const TITLE_COL = 'Title';
const CL_COL = 'CL';
const CHANGE_ID_COL = 'Change Id';
const AUTHORS_COL = 'Author(s)';
const SUBMITTED_COL = 'Submitted';
const AREA_COL = 'Area (Deprecated)';
const STATUS_COL = 'Status';
const WIP_STATUS = 'Socialization';
const DEFAULT_STATUS = 'Draft';

const BASE_URL = 'https://fuchsia-review.googlesource.com';
const RFCS_DIR = 'docs/contribute/governance/rfcs';
const RFCS_YAML_PATH = 'docs/contribute/governance/rfcs/_rfcs.yaml';
const SUBJECT_TAG = '[rfc]';

// TODO: Migrate to 'Areas' column with multiple areas once Lookup column type is supported by the Apps Script API.
const AREA_ALLOWLIST = ['bringup', 'camera', 'cast', 'chromium', 'cobalt', 'component framework',
  'connectivity', 'dart', 'developer', 'devices', 'diagnostics', 'factory',
  'fidl', 'firmware', 'flutter', 'fonts', 'general', 'governance', 'graphics',
  'hwinfo', 'identity', 'intl', 'lib', 'media', 'modular', 'power', 'recovery',
  'security', 'session', 'speech', 'storage', 'sys', 'testing', 'ui', 'virtualization',
  'zircon', 'swd', 'hci'];

function fetchRfcs() {
  // See https://gerrit-review.googlesource.com/Documentation/rest-api-changes.html for documentation.
  let url = BASE_URL + '/changes/?q='
    + 'dir:' + encodeURIComponent(RFCS_DIR)
    + '+is:open'
    + '&o=LABELS'
    + '&o=DETAILED_ACCOUNTS' //  include _account_id, email and username fields when referencing accounts.
    + '&n=100';
  Logger.log(url);
  let response = UrlFetchApp.fetch(url, { 'muteHttpExceptions': false });
  let data = JSON.parse(removeXssHeader(response));

  let old_rfcs = getExistingRfcs();

  // Accumulate create/patch requests so that requests can be batched.
  let create_rows = [];
  let patch_rows = [];
  for (let cl of data) {
    if (!cl.subject.toLowerCase().startsWith(SUBJECT_TAG)) {
      continue;
    }

    // Remove subject prefix tags (e.g. "[rfc][docs]")
    cl.subject = cl.subject.replace(/^(\S+]\s)/i, '');

    // See https://gerrit-review.googlesource.com/Documentation/rest-api-changes.html#get-content for documentation.
    let yaml = getYamlContent(cl);

    // Update old RFC
    if (cl.change_id in old_rfcs) {
      let row_name = old_rfcs[cl.change_id].name;
      let row = {
        [TITLE_COL]: cl.subject,
        [CL_COL]: `fxrev.dev/${cl._number}`,
        [AUTHORS_COL]: cl.owner.email
      };
      // Don't overwrite manually entered area with null.
      if (yaml.area) {
        row[AREA_COL] = yaml.area;
      }
      patch_rows.push({ name: row_name, values: row });
      continue;
    }

    const status = cl.work_in_progress ? WIP_STATUS : DEFAULT_STATUS;

    // Add new RFC
    create_rows.push({
      values: {
        [TITLE_COL]: cl.subject,
        [STATUS_COL]: status,
        [CL_COL]: `fxrev.dev/${cl._number}`,
        [CHANGE_ID_COL]: cl.change_id,
        [AUTHORS_COL]: cl.owner.email,
        [SUBMITTED_COL]: cl.created,
        [AREA_COL]: yaml.area
      }
    });
  }

  if (patch_rows.length != 0) {
    Area120Tables.Tables.Rows.batchUpdate({ requests: patch_rows.map(r => ({ row: r })) }, TABLE_NAME);
  }

  if (create_rows.length != 0) {
    Area120Tables.Tables.Rows.batchCreate({ requests: create_rows.map(r => ({ row: r })) }, TABLE_NAME);
  }
}

// Returns {[change_id]: {name: "row_name"}, ...}
function getExistingRfcs() {
  // In order to limit total response sizes,
  // https://developers.google.com/apps-script/advanced/tables?hl=en#read_rows_of_a_table has clients
  // read through the rows in units of pages, which we do here so that this script won't fail when
  // there are more than N RFCs
  const PAGE_SIZE = 1000;
  let response = Area120Tables.Tables.Rows.list(TABLE_NAME, { page_size: PAGE_SIZE });
  let rfcs = {};
  while (response && 'rows' in response) {
    let rows = response.rows;
    // Add existing RFCs to the object
    for (let i = 0; i < rows.length; i++) {
      let change_id = rows[i].values[CHANGE_ID_COL];
      if (change_id == null) {
        continue;
      }
      rfcs[change_id] = { name: rows[i].name };
    }
    // Read next page of rows
    let pageToken = response.nextPageToken;
    if (!pageToken) {
      response = undefined;
    } else {
      response = Area120Tables.Tables.Rows.list(TABLE_NAME, { page_size: PAGE_SIZE, page_token: pageToken });
    }
  }
  return rfcs;
}

function removeXssHeader(rawJson) {
  return rawJson.getContentText().substring(5);
}

// Returns {area: "area", status: "status"}, defaulting to null values
// if unable to parse the diff of `docs/contribute/governance/rfcs/_rfcs.yaml`.
function getYamlContent(cl) {
  let yaml_diff_url = BASE_URL
    + '/changes/'
    + cl.change_id
    + '/revisions/current/files/'
    + encodeURIComponent(RFCS_YAML_PATH)
    + '/diff'; // Get the diff only so we can parse out the relevant fields.

  let yaml_diff_res = UrlFetchApp.fetch(yaml_diff_url, { 'muteHttpExceptions': false });
  let yaml_diff_text = removeXssHeader(yaml_diff_res);
  // `yaml_diff_res_json` is the https://gerrit-review.googlesource.com/Documentation/rest-api-changes.html#diff-info
  // object, where the added text we want to pull out is nested in the array of DiffContent's `b` fields.
  let yaml_diff_res_json = JSON.parse(yaml_diff_text);
  let yaml_diff_content = yaml_diff_res_json["content"].filter(diffInfo => diffInfo.b != null);

  if (yaml_diff_content.length == 0) {
    Logger.log("No YAML change detected for RFC " + cl.subject);
    return { area: null, status: null };
  }

  let [rfc_area, rfc_status] = [null, null];
  // Loop over all the blocks of added lines, in case the author modified content elsewhere.
  for (let yaml_diff_lines of yaml_diff_content) {
    for (let line of yaml_diff_lines.b) {
      let maybe_rfc_area = line.split("area: [");
      let maybe_rfc_status = line.split("status: ");
      if (maybe_rfc_area.length == 2) {
        rfc_area = maybe_rfc_area[1].split(']')[0].replace(/\'/g, '');
      }
      // Mark provided, but 0-length status strings the same as missing status strings.
      if (maybe_rfc_status.length == 2 && maybe_rfc_status[1] != "''") {
        rfc_status = maybe_rfc_status[1].replace(/\'/g, '');
      }
    }
  }
  Logger.log('RFC YAML: ' + cl.subject + "\narea: " + rfc_area + "\nstatus: " + rfc_status);

  if (rfc_area && !AREA_ALLOWLIST.includes(rfc_area.toLowerCase())) {
    rfc_area = null;
  }

  return { area: rfc_area, status: rfc_status };
}
