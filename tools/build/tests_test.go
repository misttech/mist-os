// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package build

import (
	"encoding/json"
	"reflect"
	"testing"
)

func TestUnmarshalTest(t *testing.T) {
	manifest := `[
		 {
			"environments": [],
			"test": {
			  "cpu": "x64",
			  "label": "//garnet/bin/fuchsia:fuchsia_pkg_test_component1(//build/toolchain/fuchsia:x64)",
			  "log_settings": {
				"max_severity": "ERROR",
				"min_severity": "DEBUG"
			  },
			  "name": "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component1.cm",
			  "os": "fuchsia",
			  "package_url": "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component1.cm",
			  "package_label": "//garnet/bin/fuchsia:fuchsia_pkg(//build/toolchain/fuchsia:x64)",
			  "path": "",
			  "parallel": 2,
			  "create_no_exception_channel": true
			}
		  },
		  {
			"environments": [],
			"test": {
			  "cpu": "x64",
			  "label": "//garnet/bin/fuchsia:fuchsia_pkg(//build/toolchain/fuchsia:x64)",
			  "log_settings": {
			  },
			  "name": "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component2.cm",
			  "os": "fuchsia",
			  "package_url": "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component2.cm",
			  "path": ""
			}
		  },
		  {
			"environments": [
			  {
				"dimensions": {
				  "device_type": "QEMU"
				}
			  }
			],
			"test": {
			  "cpu": "x64",
			  "label": "//garnet/bin/fuchsia:fuchsia_pkg(//build/toolchain/fuchsia:x64)",
			  "name": "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component3.cm",
			  "os": "fuchsia",
			  "package_url": "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component3.cm",
			  "path": ""
			}
		  }
	]`
	expected := []TestSpec{
		{
			Test: Test{
				Name:                     "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component1.cm",
				PackageURL:               "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component1.cm",
				PackageLabel:             "//garnet/bin/fuchsia:fuchsia_pkg(//build/toolchain/fuchsia:x64)",
				Path:                     "",
				Label:                    "//garnet/bin/fuchsia:fuchsia_pkg_test_component1(//build/toolchain/fuchsia:x64)",
				OS:                       "fuchsia",
				LogSettings:              LogSettings{MaxSeverity: "ERROR", MinSeverity: "DEBUG"},
				RuntimeDepsFile:          "",
				Parallel:                 2,
				CPU:                      "x64",
				CreateNoExceptionChannel: true,
			},
			Envs: []Environment{},
		},
		{
			Test: Test{
				Name:            "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component2.cm",
				PackageURL:      "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component2.cm",
				Path:            "",
				Label:           "//garnet/bin/fuchsia:fuchsia_pkg(//build/toolchain/fuchsia:x64)",
				OS:              "fuchsia",
				LogSettings:     LogSettings{},
				RuntimeDepsFile: "",
				Parallel:        0,
				CPU:             "x64",
			},
			Envs: []Environment{},
		},
		{
			Test: Test{
				Name:            "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component3.cm",
				PackageURL:      "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component3.cm",
				Path:            "",
				Label:           "//garnet/bin/fuchsia:fuchsia_pkg(//build/toolchain/fuchsia:x64)",
				OS:              "fuchsia",
				LogSettings:     LogSettings{},
				RuntimeDepsFile: "",
				Parallel:        0,
				CPU:             "x64",
			},
			Envs: []Environment{
				{
					Dimensions: DimensionSet{
						"device_type": "QEMU",
					},
				},
			},
		},
	}
	var testSpecs []TestSpec
	if err := json.Unmarshal([]byte(manifest), &testSpecs); err != nil {
		t.Fatalf("error un-marshalling test specs: %v", err)
	}
	if !reflect.DeepEqual(testSpecs, expected) {
		t.Fatalf("got test spec: %#v\n\nexpected: %#v", testSpecs, expected)
	}
}

func TestIsComponentV2(t *testing.T) {
	testCases := []struct {
		name          string
		test          Test
		isComponentV2 bool
	}{
		{
			name: "component v2",
			test: Test{
				Name:       "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component.cm",
				PackageURL: "fuchsia-pkg://fuchsia.com/fuchsia_pkg#meta/component.cm",
			},
			isComponentV2: true,
		},
		{
			name: "host test",
			test: Test{
				Name: "path/to/host_test.sh",
				Path: "path/to/host_test.sh",
			},
			isComponentV2: false,
		},
	}
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			if tc.test.IsComponentV2() != tc.isComponentV2 {
				t.Errorf("failed to determine if %s is a component v2 test; want %v, got %v", tc.test.Name, tc.isComponentV2, tc.test.IsComponentV2())
			}
		})
	}
}
