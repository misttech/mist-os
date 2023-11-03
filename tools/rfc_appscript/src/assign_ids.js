// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// This file contains functionality that assigns the next available RFC number
// to an RFC. It should be called by the tracker when an RFC is approved.

const RFC_NUMBER_COL = 'RFC Number';

// Updates the given row to have the next available RFC Number. The
// given table is consulted to determine the next RFC Number.
function assignNextRfcNumber(tableId, rowId) {
  const rowName = `tables/${tableId}/rows/${rowId}`;
  const nextNumber = _getNextRFCNumber(tableId);
  console.log('Assigning ID ', nextNumber, ' to row ', rowName);

  const patchValues = {};
  patchValues[RFC_NUMBER_COL] = nextNumber;

  const response = Area120Tables.Tables.Rows.patch({ values: patchValues }, rowName);
  console.log('Response ', response);
}

// Returns the next available RFC number, as a string like 'RFC-NNNN'.
function _getNextRFCNumber(tableId) {
  return 'RFC-' + String(Math.max(..._getRFCNumbers(tableId)) + 1).padStart(4, '0');
}

// In the given table, returns an array containing 'NNNN' as a parsed integer
// for all rows whose 'RFC Number' column is of the form 'RFC-NNNN'.
function _getRFCNumbers(tableId) {
  const tableName = `tables/${tableId}`;

  let pageToken;
  const PAGE_SIZE = 1000;

  let response = Area120Tables.Tables.Rows.list(tableName, { page_size: PAGE_SIZE });

  const res = [];
  while (response) {
    const rows = response.rows;

    for (const row of rows) {
      const number = row.values[RFC_NUMBER_COL];
      if (number !== undefined && number.slice(0, 4) === 'RFC-' && !isNaN(number.slice(4))) {
        res.push(+number.slice(4));
      }
    }

    // read next page of rows
    pageToken = response.nextPageToken;
    if (!pageToken) {
      response = undefined;
    } else {
      response = Area120Tables.Tables.Rows.list(tableName, {
        page_size: PAGE_SIZE,
        page_token: pageToken,
      });
    }
  }
  return res;
}
