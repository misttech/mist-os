// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/testing/protocmp"

	fintpb "go.fuchsia.dev/fuchsia/tools/integration/fint/proto"
	"go.fuchsia.dev/fuchsia/tools/lib/osmisc"
	"go.fuchsia.dev/fuchsia/tools/lib/subprocess"
)

func TestParseArgsAndEnv(t *testing.T) {
	testCases := []struct {
		name      string
		args      []string
		env       map[string]string
		expected  setArgs
		expectErr bool
	}{
		{
			name:      "missing PRODUCT.BOARD",
			args:      []string{},
			expectErr: true,
		},
		{
			name: "valid PRODUCT.BOARD",
			args: []string{"core.x64"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				buildDir:      "out/core.x64-balanced",
				includeClippy: true,
			},
		},
		{
			name:      "invalid PRODUCT.BOARD",
			args:      []string{"corex64"},
			expectErr: true,
		},
		{
			name:      "multiple PRODUCT.BOARD args",
			args:      []string{"core.x64", "bringup.arm64"},
			expectErr: true,
		},
		{
			name: "verbose",
			args: []string{"--verbose", "core.x64"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				buildDir:      "out/core.x64-balanced",
				includeClippy: true,
				verbose:       true,
			},
		},
		{
			name: "universe packages",
			args: []string{"core.x64", "--with", "u1,u2", "--with", "u3,u4"},
			expected: setArgs{
				product:          "core",
				board:            "x64",
				buildDir:         "out/core.x64-balanced",
				includeClippy:    true,
				universePackages: []string{"u1", "u2", "u3", "u4"},
			},
		},
		{
			name:      "base packages not allowed",
			args:      []string{"core.x64", "--with-base", "u1"},
			expectErr: true,
		},
		{
			name:      "cache packages not allowed",
			args:      []string{"core.x64", "--with-cache", "u1"},
			expectErr: true,
		},
		{
			name: "tests",
			args: []string{"core.x64", "--with-test", "b1,b2", "--with-test", "b3,b4"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				buildDir:      "out/core.x64-balanced",
				includeClippy: true,
				testLabels:    []string{"b1", "b2", "b3", "b4"},
			},
		},
		{
			name: "host labels",
			args: []string{"core.x64", "--with-host", "h1,h2", "--with-host", "h3,h4"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				buildDir:      "out/core.x64-balanced",
				includeClippy: true,
				hostLabels:    []string{"h1", "h2", "h3", "h4"},
			},
		},
		{
			name: "fuzz-with",
			args: []string{"core.x64", "--fuzz-with", "asan,ubsan", "--fuzz-with", "kasan"},
			expected: setArgs{
				product:        "core",
				board:          "x64",
				buildDir:       "out/core.x64-balanced",
				includeClippy:  true,
				fuzzSanitizers: []string{"asan", "ubsan", "kasan"},
			},
		},
		{
			name: "gn args",
			args: []string{"core.x64", "--args", `foo=["bar", "baz"]`, "--args", "x=5"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				buildDir:      "out/core.x64-balanced",
				includeClippy: true,
				// --args values shouldn't be split at commas, since commas can
				// be part of the args themselves.
				gnArgs: []string{`foo=["bar", "baz"]`, "x=5"},
			},
		},
		{
			name: "fint params path",
			env: map[string]string{
				// fx sets this env var if the top-level --dir flag is set.
				buildDirEnvVar: "out/repro42",
			},
			args: []string{
				"--fint-params-path", "foo.fint.textproto",
			},
			expected: setArgs{
				fintParamsPath: "foo.fint.textproto",
				includeClippy:  true,
				buildDir:       "out/repro42",
			},
		},
		{
			name: "absolute build dir env var",
			env: map[string]string{
				checkoutDirEnvVar: "/abs/path/to/fuchsia/dir",
				buildDirEnvVar:    "/abs/path/to/fuchsia/dir/out/ninja",
			},
			args: []string{
				"--fint-params-path", "bar.fint.textproto",
			},
			expected: setArgs{
				fintParamsPath: "bar.fint.textproto",
				includeClippy:  true,
				checkoutDir:    "/abs/path/to/fuchsia/dir",
				buildDir:       "out/ninja",
			},
		},
		{
			name: "build dir not under checkout dir",
			env: map[string]string{
				checkoutDirEnvVar: "/abs/path/to/fuchsia/dir",
				buildDirEnvVar:    "/somewhere/else/out/samurai",
			},
			args: []string{
				"--fint-params-path", "bar.fint.textproto",
			},
			expectErr: true,
		},
		{
			name:      "rejects --cxx-rbe and --no-cxx-rbe",
			args:      []string{"core.x64", "--cxx-rbe", "--no-cxx-rbe"},
			expectErr: true,
		},
		{
			name:      "rejects --ccache and --no-ccache",
			args:      []string{"core.x64", "--ccache", "--no-ccache"},
			expectErr: true,
		},
		{
			name: "honors top-level fx --dir flag",
			args: []string{"core.x64"},
			env: map[string]string{
				checkoutDirEnvVar: "/usr/foo/fuchsia",
				// fx sets this env var if the top-level --dir flag is set.
				buildDirEnvVar: "/usr/foo/fuchsia/out/foo",
			},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				includeClippy: true,
				checkoutDir:   "/usr/foo/fuchsia",
				buildDir:      "out/foo",
			},
		},
		{
			name: "autodir",
			args: []string{"bringup.arm64"},
			expected: setArgs{
				product:       "bringup",
				board:         "arm64",
				includeClippy: true,
				buildDir:      "out/bringup.arm64-balanced",
			},
		},
		{
			name: "autodir with variants",
			args: []string{"core.x64", "--variant", "profile,kasan", "--variant", "ubsan"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				includeClippy: true,
				buildDir:      "out/core.x64-profile-kasan-ubsan-balanced",
				variants:      []string{"profile", "kasan", "ubsan"},
			},
		},
		{
			name: "autodir and release",
			args: []string{"core.x64", "--variant", "foo", "--release"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				includeClippy: true,
				isRelease:     true,
				buildDir:      "out/core.x64-foo-release",
				variants:      []string{"foo"},
			},
		},
		{
			name:      "autodir with complex variants",
			args:      []string{"core.x64", "--variant", "asan-fuzzer/foo"},
			expectErr: true,
		},
		{
			name: "simple boolean flags",
			args: []string{"core.x64", "--include-clippy=false", "--cargo-toml-gen"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				buildDir:      "out/core.x64-balanced",
				includeClippy: false,
				cargoTOMLGen:  true,
			},
		},
		{
			name: "ide files",
			args: []string{"core.x64", "--ide", "json,vs"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				buildDir:      "out/core.x64-balanced",
				includeClippy: true,
				ideFiles:      []string{"json", "vs"},
			},
		},
		{
			name: "json ide scripts",
			args: []string{"core.x64", "--json-ide-script", "//foo.py", "--json-ide-script", "//bar.py"},
			expected: setArgs{
				product:        "core",
				board:          "x64",
				buildDir:       "out/core.x64-balanced",
				jsonIDEScripts: []string{"//foo.py", "//bar.py"},
				includeClippy:  true,
			},
		},
		{
			name: "RBE mode auto",
			args: []string{"core.x64", "--rbe-mode=auto"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				buildDir:      "out/core.x64-balanced",
				includeClippy: true,
				rbeMode:       "auto",
			},
		},
		{
			name: "RBE mode off",
			args: []string{"core.x64", "--rbe-mode=off"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				buildDir:      "out/core.x64-balanced",
				includeClippy: true,
				rbeMode:       "off",
			},
		},
		{
			name: "RBE mode cloudtop",
			args: []string{"core.x64", "--rbe-mode=cloudtop"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				buildDir:      "out/core.x64-balanced",
				includeClippy: true,
				rbeMode:       "cloudtop",
			},
		},
		{
			name: "Debug compilation mode",
			args: []string{"core.x64", "--debug"},
			expected: setArgs{
				product:       "core",
				board:         "x64",
				buildDir:      "out/core.x64-debug",
				includeClippy: true,
				isDebug:       true,
			},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			if _, ok := tc.env[checkoutDirEnvVar]; !ok {
				checkoutDir := t.TempDir()
				tc.expected.checkoutDir = checkoutDir
				if tc.env == nil {
					tc.env = make(map[string]string)
				}
				tc.env[checkoutDirEnvVar] = checkoutDir
			}
			if tc.expected.rbeMode == "" {
				tc.expected.rbeMode = defaultRbeMode
			}
			cmd, err := parseArgsAndEnv(tc.args, tc.env)
			if err != nil {
				if !tc.expectErr {
					t.Fatalf("Parse args error: %s", err)
				}
				return
			} else if tc.expectErr {
				t.Fatalf("Expected parse args error but parsing succeeded")
			}

			opts := []cmp.Option{cmpopts.EquateEmpty(), cmp.AllowUnexported(setArgs{})}
			if diff := cmp.Diff(&tc.expected, cmd, opts...); diff != "" {
				t.Fatalf("Unexpected arg parse result (-want +got):\n%s", diff)
			}
		})
	}
}

type fakeSubprocessRunner struct {
	fail bool
}

func (r fakeSubprocessRunner) Run(context.Context, []string, subprocess.RunOptions) error {
	if r.fail {
		return &exec.ExitError{}
	}
	return nil
}

func TestConstructStaticSpec(t *testing.T) {
	rbeSupported := rbeIsSupported()
	const (
		rbe_mode_off = `rbe_mode="off"`
	)
	testCases := []struct {
		name         string
		args         *setArgs
		runner       fakeSubprocessRunner
		expected     *fintpb.Static
		expectErr    bool
		cannotUseRbe bool
	}{
		{
			name: "basic",
			args: &setArgs{
				board:            "arm64",
				product:          "bringup",
				isRelease:        true,
				universePackages: []string{"universe"},
				hostLabels:       []string{"host"},
				variants:         []string{"variant"},
				ideFiles:         []string{"json"},
				jsonIDEScripts:   []string{"foo.py"},
				gnArgs:           []string{"args"},
				disableCxxRbe:    true,
			},
			expected: &fintpb.Static{
				Board:            "boards/arm64.gni",
				Product:          "products/bringup.gni",
				CompilationMode:  fintpb.Static_COMPILATION_MODE_RELEASE,
				UniversePackages: []string{"universe"},
				HostLabels:       []string{"host"},
				Variants:         []string{"variant"},
				IdeFiles:         []string{"json"},
				JsonIdeScripts:   []string{"foo.py"},
				GnArgs:           []string{"args", rbe_mode_off},
			},
		},
		{
			name: "ccache enabled",
			args: &setArgs{
				useCcache: true,
			},
			expected: &fintpb.Static{
				GnArgs: []string{"use_ccache=true", rbe_mode_off},
			},
		},
		{
			name:     "cxx-rbe default with access",
			args:     &setArgs{},
			expected: &fintpb.Static{
				// C++ is configured by the RBE mode
			},
		},
		{
			name: "cxx-rbe default without access",
			args: &setArgs{},
			expected: &fintpb.Static{
				CxxRbeEnable: false,
			},
			cannotUseRbe: true,
		},
		{
			name: "cxx-rbe disabled by RBE mode off",
			args: &setArgs{rbeMode: "off"},
			expected: &fintpb.Static{
				GnArgs: []string{rbe_mode_off},
			},
		},
		{
			name: "cxx-rbe enabled",
			args: &setArgs{enableCxxRbe: true},
			expected: &fintpb.Static{
				CxxRbeEnable: true,
			},
			expectErr: !rbeSupported,
		},
		{
			// allow the setting, but gives a notice about access
			name: "cxx-rbe enabled without access",
			args: &setArgs{enableCxxRbe: true},
			expected: &fintpb.Static{
				CxxRbeEnable: true,
			},
			expectErr:    !rbeSupported,
			cannotUseRbe: true,
		},
		{
			name: "cxx-rbe disabled",
			args: &setArgs{disableCxxRbe: true},
			expected: &fintpb.Static{
				CxxRbeEnable: false,
			},
		},
		{
			name: "link-rbe default",
			args: &setArgs{disableCxxRbe: true},
			expected: &fintpb.Static{
				LinkRbeEnable: false,
			},
		},
		{
			name: "link-rbe enabled",
			args: &setArgs{disableCxxRbe: true, enableLinkRbe: true},
			expected: &fintpb.Static{
				LinkRbeEnable: true,
			},
			expectErr: !rbeSupported,
		},
		{
			name: "rust-rbe default",
			args: &setArgs{disableCxxRbe: true},
			expected: &fintpb.Static{
				RustRbeEnable: false,
			},
		},
		{
			name: "rust-rbe enabled",
			args: &setArgs{disableCxxRbe: true, enableRustRbe: true},
			expected: &fintpb.Static{
				RustRbeEnable: true,
			},
			expectErr: !rbeSupported,
		},
		{
			name: "bazel-rbe default",
			args: &setArgs{disableCxxRbe: true},
			expected: &fintpb.Static{
				BazelRbeEnable: false,
			},
		},
		{
			name: "bazel-rbe enabled",
			args: &setArgs{disableCxxRbe: true, enableBazelRbe: true},
			expected: &fintpb.Static{
				BazelRbeEnable: true,
			},
			expectErr: !rbeSupported,
		},
		{
			name: "build_event_service enabled",
			args: &setArgs{disableCxxRbe: true, buildEventService: "sponge"},
			expected: &fintpb.Static{
				BuildEventService: "sponge",
				GnArgs:            []string{`bazel_upload_build_events = "sponge"`, rbe_mode_off},
			},
		},
		{
			name: "fuzzer variants",
			args: &setArgs{
				fuzzSanitizers: []string{"asan", "ubsan"},
				disableCxxRbe:  true,
			},
			expected: &fintpb.Static{
				Variants: append(fuzzerVariants("asan"), fuzzerVariants("ubsan")...),
			},
		},
		{
			name: "cargo toml gen",
			args: &setArgs{
				cargoTOMLGen:  true,
				disableCxxRbe: true,
			},
			expected: &fintpb.Static{
				HostLabels: []string{"//build/rust:cargo_toml_gen"},
			},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			if tc.args.board == "" {
				tc.args.board = "x64"
			}
			if tc.args.product == "" {
				tc.args.product = "core"
			}
			if tc.args.rbeMode == "" {
				tc.args.rbeMode = "off"
			}

			expected := &fintpb.Static{
				Board:             "boards/x64.gni",
				Product:           "products/core.gni",
				CompilationMode:   fintpb.Static_COMPILATION_MODE_BALANCED,
				ExportRustProject: true,
			}
			proto.Merge(expected, tc.expected)

			if len(expected.GnArgs) == 0 {
				expected.GnArgs = []string{rbe_mode_off}
			}

			checkoutDir := t.TempDir()
			createFile(t, checkoutDir, expected.Board)
			createFile(t, checkoutDir, expected.Product)

			got, err := constructStaticSpec(checkoutDir, tc.args, !tc.cannotUseRbe)
			if err != nil {
				if tc.expectErr {
					return
				} else {
					t.Fatal(err)
				}
			} else if tc.expectErr {
				t.Errorf("Expected an error, but got nil.")
			}

			if diff := cmp.Diff(expected, got, protocmp.Transform()); diff != "" {
				t.Fatalf("Static spec from args is wrong (-want +got):\n%s", diff)
			}
		})
	}
}

func TestFindGNIFile(t *testing.T) {
	t.Run("finds file in root boards directory", func(t *testing.T) {
		checkoutDir := t.TempDir()
		path := filepath.Join("boards", "core.gni")
		createFile(t, checkoutDir, path)

		wantPath := "boards/core.gni"
		gotPath, err := findGNIFile(checkoutDir, "boards", "core")
		if err != nil {
			t.Fatal(err)
		}
		if wantPath != gotPath {
			t.Errorf("findGNIFile returned wrong path: want %s, got %s", wantPath, gotPath)
		}
	})

	t.Run("finds file in root products/tests directory", func(t *testing.T) {
		checkoutDir := t.TempDir()
		path := filepath.Join("products", "tests", "core.gni")
		createFile(t, checkoutDir, path)

		wantPath := "products/tests/core.gni"
		gotPath, err := findGNIFile(checkoutDir, filepath.Join("products", "tests"), "core")
		if err != nil {
			t.Fatal(err)
		}
		if wantPath != gotPath {
			t.Errorf("findGNIFile returned wrong path: want %s, got %s", wantPath, gotPath)
		}
	})

	t.Run("file in root products directory shadows root product/tests directory", func(t *testing.T) {
		checkoutDir := t.TempDir()
		path := filepath.Join("products", "core.gni")
		createFile(t, checkoutDir, path)
		shadowedPath := filepath.Join("products", "tests", "core.gni")
		createFile(t, checkoutDir, shadowedPath)

		wantPath := "products/core.gni"
		gotPath, err := findGNIFile(checkoutDir, "products", "core")
		if err != nil {
			t.Fatal(err)
		}
		if wantPath != gotPath {
			t.Errorf("findGNIFile returned wrong path: want %s, got %s", wantPath, gotPath)
		}
	})

	t.Run("checks vendor directories first", func(t *testing.T) {
		checkoutDir := t.TempDir()
		path := filepath.Join("vendor", "foo", "boards", "core.gni")
		createFile(t, checkoutDir, path)
		// Even if a matching file exists in the root "//boards" directory, we
		// should prefer the file from the vendor directory.
		createFile(t, checkoutDir, "boards", "core.gni")

		gotPath, err := findGNIFile(checkoutDir, "boards", "core")
		if err != nil {
			t.Fatal(err)
		}
		if path != gotPath {
			t.Errorf("findGNIFile returned wrong path: want %s, got %s", path, gotPath)
		}
	})

	t.Run("checks vendor product directories first", func(t *testing.T) {
		checkoutDir := t.TempDir()
		path := filepath.Join("vendor", "foo", "products", "tests", "core.gni")
		createFile(t, checkoutDir, path)
		// Even if a matching file exists in the root "//products/tests" directory, we
		// should prefer the file from the vendor directory.
		createFile(t, checkoutDir, "products", "tests", "core.gni")

		gotPath, err := findGNIFile(checkoutDir, filepath.Join("products", "tests"), "core")
		if err != nil {
			t.Fatal(err)
		}
		if path != gotPath {
			t.Errorf("findGNIFile returned wrong path: want %s, got %s", path, gotPath)
		}
	})

	t.Run("returns an error if file doesn't exist", func(t *testing.T) {
		checkoutDir := t.TempDir()
		createFile(t, checkoutDir, "boards", "core.gni")

		if path, err := findGNIFile(checkoutDir, "boards", "doesnotexist"); err == nil {
			t.Fatalf("Expected findGNIFile to fail given a nonexistent board but got path: %s", path)
		}
	})
}

func createFile(t *testing.T, pathParts ...string) {
	t.Helper()

	path := filepath.Join(pathParts...)
	f, err := osmisc.CreateFile(path)
	if err != nil {
		t.Fatal(err)
	}
	f.Close()

	t.Cleanup(func() { os.Remove(path) })
}
