// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package build

import "strings"

const (
	componentV2Suffix = ".cm"
)

// TestSpec is the specification for a single test and the environments it
// should be executed in.
type TestSpec struct {
	// Test is the test that this specification is for.
	Test `json:"test"`

	// Envs is a set of environments that the test should be executed in.
	Envs []Environment `json:"environments"`

	// ImageOverrides is a map of the images to override the default values in
	// images.json used to boot a target. The key should be an ImageOverrideType
	// and the value should be the label of the image to override with as defined
	// in images.json.
	ImageOverrides ImageOverrides `json:"image_overrides,omitempty"`
}

// Test encapsulates details about a particular test.
type Test struct {
	// Name is a human-readable identifier for this test. It should be unique
	// within any given TestSpec.
	Name string `json:"name"`

	// PackageURL is the fuchsia package URL for this test. It is only set for
	// tests targeting Fuchsia.
	PackageURL string `json:"package_url,omitempty"`

	// PackageLabel is the full GN label with toolchain of the fuchsia package
	// for this test.
	PackageLabel string `json:"package_label,omitempty"`

	// PackageManifests is a list of paths to manifests describing the
	// packages needed by the test. They are all relative to the build
	// directory.
	PackageManifests []string `json:"package_manifests,omitempty"`

	// Path is the path to the test on the target OS.
	Path string `json:"path"`

	// Label is the full GN label with toolchain for the test target.
	// E.g.: //src/foo/tests:foo_tests(//build/toolchain/fuchsia:x64)
	Label string `json:"label"`

	// OS is the operating system in which this test must be executed.
	OS string `json:"os"`

	// CPU architecture on which this test can execute.
	CPU string `json:"cpu"`

	// Settings of log produced by this test
	LogSettings LogSettings `json:"log_settings,omitempty"`

	// Number of test cases to run in parallel. This only works with v2 tests.
	Parallel uint16 `json:"parallel,omitempty"`

	// RuntimeDepsFile is a relative path within the build directory to a file
	// containing a JSON list of the test's runtime dependencies, Currently this
	// field only makes sense for Linux and Mac tests.
	RuntimeDepsFile string `json:"runtime_deps,omitempty"`

	// Isolated specifies whether the test should run in its own shard.
	Isolated bool `json:"isolated,omitempty"`

	// TimeoutSecs is the timeout for the test.
	TimeoutSecs int `json:"timeout_secs,omitempty"`
}

// IsComponentV2 returns whether the test is a component v2 test.
func (t *Test) IsComponentV2() bool {
	return strings.HasSuffix(t.PackageURL, componentV2Suffix)
}

type LogSettings struct {
	// Max severity of logs produced by the test. Message more severe than this value will cause the
	// test to fail.
	MaxSeverity string `json:"max_severity,omitempty"`
	// Min severity of logs produced by the test. Messages less severe than this value will not be
	// printed.
	MinSeverity string `json:"min_severity,omitempty"`
}

// Environment describes the full environment a test requires.
// The GN environments specified by test authors in the Fuchsia source
// correspond directly to the Environment struct defined here.
type Environment struct {
	// Dimensions gives the Swarming dimensions a test wishes to target.
	Dimensions DimensionSet `json:"dimensions"`

	// Tags are keys given to an environment on which the testsharder may filter.
	Tags []string `json:"tags,omitempty"`

	// ServiceAccount gives a service account to attach to Swarming task.
	ServiceAccount string `json:"service_account,omitempty"`

	// Netboot tells whether to "netboot" instead of paving before running the tests.
	Netboot bool `json:"netboot,omitempty"`
}

func (env Environment) TargetsEmulator() bool {
	return env.Dimensions.DeviceType() == "QEMU" || env.Dimensions.DeviceType() == "AEMU"
}

// ImageOverrides gives images by label that should override the default images.
type ImageOverrides struct {
	ZBI        string `json:"zbi,omitempty"`
	VBMeta     string `json:"vbmeta,omitempty"`
	QEMUKernel string `json:"qemu_kernel,omitempty"`
	FVM        string `json:"fvm,omitempty"`
	Fxfs       string `json:"fxfs,omitempty"`

	// EFIDisk is the label of a bootable, UEFI (FAT) filesystem or disk image.
	EFIDisk string `json:"efi_disk,omitempty"`
}

func (o ImageOverrides) IsEmpty() bool {
	return o.ZBI == "" && o.VBMeta == "" && o.QEMUKernel == "" && o.EFIDisk == "" && o.FVM == ""
}

// DimensionSet encapsulates the Swarming dimensions a test wishes to target.
type DimensionSet map[string]string

// DeviceType represents the class of device the test should run on.  This
// is a required field.
func (ds DimensionSet) DeviceType() string {
	return ds["device_type"]
}

// The OS to run the test on (e.g., "Linux" or "Mac"). Used for host-side testing.
func (ds DimensionSet) OS() string {
	return ds["os"]
}

// CPU is architecture that the test is meant to run on.
func (ds DimensionSet) CPU() string {
	return ds["cpu"]
}

// Testbed denotes a physical test device configuration to run a test on (e.g., multi-device set-ups or devices inside chambers for connectivity testing).
func (ds DimensionSet) Testbed() string {
	return ds["testbed"]
}

// Pool denotes the swarming pool to run a test in.
func (ds DimensionSet) Pool() string {
	return ds["pool"]
}
