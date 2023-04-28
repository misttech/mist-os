// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package result

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	spdx_common "github.com/spdx/tools-golang/spdx/common"
	spdx "github.com/spdx/tools-golang/spdx/v2_2"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project"
)

var (
	testDataDir = flag.String("test_data_dir", "", "Path to test data directory")
)

// If the root project is null, SPDX doc generation should fail.
func TestDocCreationEmpty(t *testing.T) {
	dir := t.TempDir()
	Config = &ResultConfig{
		FuchsiaDir: dir,
		OutDir:     dir,
	}
	_, err := generateSPDXDoc(t.Name(), []*project.Project{}, nil)
	if err == nil {
		t.Fatalf("%s: expected error, got none.", t.Name())
	}
}

// Simple case: Ensure we can generate a simple doc with one SPDX package.
func TestDocCreationOnePackage(t *testing.T) { runTest("one_package", t) }

// Ensure we can create an SPDX document with multiple packages.
// The root package must have a "CONTAINS" relationship on all other packages.
func TestDocCreationMultiPackage(t *testing.T) { runTest("multi_package", t) }

// Ensure license information is presented properly in the SPDX document.
func TestDocCreationMultiPackageOneLicense(t *testing.T) { runTest("multi_package_one_license", t) }

// If a given package has multiple license files, ensure they are all included in
// the SPDX document successfully.
func TestDocCreationMultiPackageMultiLicense(t *testing.T) { runTest("multi_package_multi_license", t) }

// Similar to the above, NOTICE files have multiple license texts. Ensure we
// handle that situation properly.
func TestDocCreationMultiPackageNotice(t *testing.T) { runTest("multi_package_one_notice", t) }

func runTest(folder string, t *testing.T) {
	dir := t.TempDir()
	Config = &ResultConfig{
		FuchsiaDir: dir,
		OutDir:     dir,
	}
	projectsJSONPath := filepath.Join(*testDataDir, "spdx", folder, "filtered_projects.json")
	projects := loadProjectsJSON(projectsJSONPath, t)
	root := projects[0]

	_, err := generateSPDXDoc(t.Name(), projects, root)
	if err != nil {
		t.Fatalf("%s: expected no error, got %v", t.Name(), err)
	}

	wantPath := filepath.Join(*testDataDir, "spdx", folder, "want.json")
	gotPath := filepath.Join(dir, spdxFilename)
	want, got := loadWantGot(wantPath, gotPath, t)

	if d := cmp.Diff(want, got); d != "" {
		t.Errorf("%v: compare docs mismatch: (-want +got):\n%s", t.Name(), d)
	}
}

func loadProjectsJSON(path string, t *testing.T) []*project.Project {
	t.Helper()

	content, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("%s: failed to read in json file [%s]: %v", t.Name(), path, err)
	}

	var projects []*project.Project
	err = json.Unmarshal(content, &projects)
	if err != nil {
		t.Fatalf("%s: failed to unmarshal projects data [%s]: %v", t.Name(), path, err)
	}
	return projects
}

func loadWantGot(wantPath, gotPath string, t *testing.T) (*spdx.Document, *spdx.Document) {
	t.Helper()

	content, err := os.ReadFile(wantPath)
	if err != nil {
		t.Fatalf("%s: failed to read in json file [%s]: %v", t.Name(), wantPath, err)
	}

	var want *spdx.Document
	err = json.Unmarshal(content, &want)
	if err != nil {
		t.Fatalf("%s: failed to unmarshal data [%s]: %v", t.Name(), wantPath, err)
	}

	content, err = os.ReadFile(gotPath)
	if err != nil {
		t.Fatalf("%s: failed to read in json file [%s]: %v", t.Name(), gotPath, err)
	}

	var got *spdx.Document
	err = json.Unmarshal(content, &got)
	if err != nil {
		t.Fatalf("%s: failed to unmarshal data [%s]: %v", t.Name(), gotPath, err)
	}

	// Unable to accurately set the creation timestamp, so skip this field.
	got.CreationInfo.Created = "SKIP ME"

	// JSON unmarshalling of the SPDX DocElementID is broken.
	// I will file a bug against the open source project.
	//   https://github.com/spdx/tools-golang/blob/main/spdx/common/identifier.go#L100
	for _, r := range got.Relationships {
		r.RefA.ElementRefID = spdx_common.ElementID(fmt.Sprintf("%s\"\n      }", r.RefA.ElementRefID))
		r.RefB.ElementRefID = spdx_common.ElementID(fmt.Sprintf("%s\"\n      }", r.RefB.ElementRefID))
	}

	return want, got
}
