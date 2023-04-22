// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ffx

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

// createScript returns the path to a bash script that output the given content.
func createScript(t *testing.T, contents string) string {
	name := filepath.Join(t.TempDir(), "ffxtool.sh")
	contents = `#!/bin/bash
	echo '` + contents + `'`
	if err := os.WriteFile(name, []byte(contents), 0o700); err != nil {
		t.Fatal(err)
	}
	return name
}

func TestTargetListEmpty(t *testing.T) {
	data, err := json.Marshal([]targetEntry{})
	if err != nil {
		t.Fatalf("Failed to marshal: %s", err)
	}
	ffxtoolScript := createScript(t, string(data))
	ffx, err := NewFFXTool(ffxtoolScript)
	if err != nil {
		t.Fatalf("Failed to create ffx tool: %s", err)
	}
	entries, err := ffx.TargetList(context.Background())
	if err != nil {
		t.Fatalf("Failed to run target list: %s", err)
	}
	if len(entries) != 0 {
		t.Fatalf("entries not empty: %v", entries)
	}
}

func TestTargetList(t *testing.T) {
	expected_entries := []targetEntry{
		{NodeName: "1", Addresses: []string{"127.0.0.1"}, TargetState: "Product"},
		{NodeName: "2", Addresses: []string{"127.0.0.1"}, TargetState: "Product"},
	}
	data, err := json.Marshal(expected_entries)
	if err != nil {
		t.Fatalf("Failed to marshal: %s", err)
	}
	ffxtoolScript := createScript(t, string(data))
	ffx, err := NewFFXTool(ffxtoolScript)
	if err != nil {
		t.Fatalf("Failed to create ffx tool: %s", err)
	}
	entries, err := ffx.TargetList(context.Background())
	if err != nil {
		t.Fatalf("Failed to run target list: %s", err)
	}
	if diff := cmp.Diff(entries, expected_entries); diff != "" {
		t.Fatalf("unexpected entries, diff:\n%s", diff)
	}

	entries, err = ffx.TargetListForNode(context.Background(), []string{"1"})
	if err != nil {
		t.Fatalf("Failed to run target list: %s", err)
	}
	if diff := cmp.Diff(entries, expected_entries[:1]); diff != "" {
		t.Fatalf("unexpected entries, diff:\n%s", diff)
	}
}
