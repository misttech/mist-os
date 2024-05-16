// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// This file contains javascript which gets deployed to scripts.google.com, in
// order to power custom logic for the internal RFC tracker.

const TITLE_COL = 'Title';
const CL_COL = 'CL';
const CHANGE_ID_COL = 'Change Id';
const AUTHORS_COL = 'Author(s)';
const SUBMITTED_COL = 'Submitted';
const STATUS_COL = 'Status';
const NEEDS_SYNC_COL = 'Needs Sync';
const CL_UPDATED_COL = 'CL Updated';

const WIP_STATUS = 'Socialization';
const DEFAULT_STATUS = 'Draft';
const ABANDONED_STATUS = 'Withdrawn';

const RFCS_DIR = 'docs/contribute/governance/rfcs';
const SUBJECT_TAG = '[rfc]';

// Run _fetchRfcs() on the main 'prod' tracker.
function fetchRfcs() {
  _fetchRfcs('bbfvcDiuyZ08S5ZJ7vN4ZI');
}

// Run _fetchRfcs() on a test copy of the tracker.
function fetchRfcsTest() {
  // Put your personal test Table's ID here:
  _fetchRfcs('');
}

// Gets the list of open RFC CLs from gerrit and compares that to the set of
// RFCs in the tracker. For any CLs missing from the tracker, creates stub rows
// with just the change_id filled in.
function _fetchRfcs(tableId) {
  const gerritCls = _fetchOpenRfcsCls();
  const trackerCls = _getExistingRfcs(tableId);

  // Accumulate create requests so that requests can be batched.
  let createRows = [];
  for (const changeId of gerritCls) {
    if (!(changeId in trackerCls)) {
      createRows.push({
        row: {
          values: {
            [CHANGE_ID_COL]: changeId,
          }
        }
      });
    }
  }

  if (createRows.length != 0) {
    Area120Tables.Tables.Rows.batchCreate({ requests: createRows }, `tables/${tableId}`);
  }
}

// Returns an Array of change_ids.
function _fetchOpenRfcsCls() {
  const data = parseGerritResponse(UrlFetchApp.fetch(
    GERRIT_API_URL + '/changes/?q='
    + 'dir:' + encodeURIComponent(RFCS_DIR)
    + '+is:open'
    + '&n=100'));

  const changeIds = [];

  for (const cl of data) {
    if (cl.subject.toLowerCase().startsWith(SUBJECT_TAG)) {
      changeIds.push(cl.change_id);
    }
  }

  return changeIds;
}

// Returns {[change_id]: {name: 'row_name'}, ...}
function _getExistingRfcs(tableId) {
  // In order to limit total response sizes,
  // https://developers.google.com/apps-script/advanced/tables?hl=en#read_rows_of_a_table has clients
  // read through the rows in units of pages, which we do here so that this script won't fail when
  // there are more than N RFCs
  const PAGE_SIZE = 1000;
  let response = Area120Tables.Tables.Rows.list(`tables/${tableId}`, { page_size: PAGE_SIZE });
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
      response = Area120Tables.Tables.Rows.list(`tables/${tableId}`, { page_size: PAGE_SIZE, page_token: pageToken });
    }
  }
  return rfcs;
}

// Apply any programmatic updates we might want to make to the given row.
// Specifically, fetch the latest state from gerrit and update the row to
// match.
function syncRow(tableId, rowId) {
  const rowName = `tables/${tableId}/rows/${rowId}`;
  const row = Area120Tables.Tables.Rows.get(rowName);

  console.log('Row before: ', row);

  const changeId = row.values[CHANGE_ID_COL];

  const cl = parseGerritResponse(UrlFetchApp.fetch(
    // include _account_id, email and username fields when referencing accounts.
    GERRIT_API_URL + `/changes/${changeId}?o=DETAILED_ACCOUNTS`
  ));

  // Remove subject prefix tags (e.g. '[rfc][docs]')
  const title = cl.subject.replace(/^(\S+]\s)/i, '');

  const patchedValues = {
    [TITLE_COL]: title,
    [CL_COL]: `fxrev.dev/${cl._number}`,
    [AUTHORS_COL]: cl.owner.email,
    [SUBMITTED_COL]: cl.created,
    [CL_UPDATED_COL]: cl.updated,

    [NEEDS_SYNC_COL]: false,
  };

  if (cl.work_in_progress) {
    if (!row.values[STATUS_COL]) {
      patchedValues[STATUS_COL] = WIP_STATUS;
    }
  } else {
    if (!row.values[STATUS_COL] || row.values[STATUS_COL] === WIP_STATUS) {
      patchedValues[STATUS_COL] = DEFAULT_STATUS;
    }
  }

  // Abandoning the CL amounts to withdrawing the RFC.
  if (cl.status === 'ABANDONED') {
    patchedValues[STATUS_COL] = ABANDONED_STATUS;
  }

  Area120Tables.Tables.Rows.patch({ values: patchedValues }, rowName);
}

// Returns {[change_id]: true, ...}
function _getExistingRfcsFromAppSheet() {
  const rows = callAppSheetAPI({
    "Action": "Find",
    "Properties": {},
  });

  const res = {};
  for (const row of rows) {
    res[row['Change ID']] = true;
  }

  return res;
}

// Gets the list of open RFC CLs from gerrit and compares that to the set of
// RFCs in the AppSheet tracker. For any CLs missing from the tracker, creates
// stub rows with just the change_id filled in.
function addNewRfcsToAppSheet() {
  const gerritCls = _fetchOpenRfcsCls();
  const trackerCls = _getExistingRfcsFromAppSheet();

  // Accumulate create requests so that requests can be batched.
  let createRows = [];
  for (const changeId of gerritCls) {
    if (!(changeId in trackerCls)) {
      createRows.push({ [CHANGE_ID_COL]: changeId });
    }
  }

  if (createRows.length != 0) {
    appScriptApiCall({
      "Action": "Add",
      "Properties": {},
      Rows: createRows,
    });
  }
}

// Call the gerrit API and return some specific information about the named CL.
function getClInfo(changeId) {
  const cl = parseGerritResponse(UrlFetchApp.fetch(
    // include _account_id, email and username fields when referencing accounts.
    GERRIT_API_URL + `/changes/${changeId}?o=DETAILED_ACCOUNTS`
  ));

  // Remove subject prefix tags (e.g. '[rfc][docs]')
  const title = cl.subject.replace(/^(\S+]\s)/i, '');

  return {
    title,
    number: cl._number,
    author: cl.owner.email,
    created: cl.created,
    updated: cl.updated,
    status: cl.status,
    work_in_progress: Boolean(cl.work_in_progress),
  };
}
