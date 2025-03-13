// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ffxutil

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"go.fuchsia.dev/fuchsia/tools/bootserver"
	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/lib/clock"
)

func TestFFXInstance(t *testing.T) {
	tmpDir := t.TempDir()
	ffxPath := filepath.Join(tmpDir, "ffx")
	if err := os.WriteFile(ffxPath, []byte("#!/bin/bash\necho $@"), os.ModePerm); err != nil {
		t.Fatal("failed to write mock ffx tool")
	}
	fakeClock := clock.NewFakeClock()
	ctx := clock.NewContext(context.Background(), fakeClock)
	ffx, _ := NewFFXInstance(ctx, ffxPath, tmpDir, []string{}, "target", filepath.Join(tmpDir, "sshKey"), filepath.Join(tmpDir, "out"), UseFFXLegacy)

	var buf []byte
	stdout := bytes.NewBuffer(buf)
	ffx.SetStdoutStderr(stdout, stdout)

	assertRunsExpectedCmd := func(runErr error, stdout *bytes.Buffer, expectedCmd string) {
		if runErr != nil {
			t.Errorf("failed to run cmd: %s", runErr)
		}
		stdoutStr := stdout.String()
		if !strings.HasSuffix(strings.TrimSpace(stdoutStr), expectedCmd) {
			t.Errorf("got %q, want %q", stdoutStr, expectedCmd)
		}
	}
	assertRunsExpectedCmd(ffx.List(ctx), stdout, "target list")

	assertRunsExpectedCmd(ffx.TargetWait(ctx), stdout, "--target target target wait")

	// Create a new instance that uses the same ffx config but runs against a different target.
	ffx2 := FFXWithTarget(ffx, "target2")
	var buf2 []byte
	stdout2 := bytes.NewBuffer(buf2)
	ffx2.SetStdoutStderr(stdout2, stdout2)
	assertRunsExpectedCmd(ffx2.TargetWait(ctx), stdout2, "--target target2 target wait")

	// Test expects a run_summary.json to be written in the test output directory.
	outDir := filepath.Join(tmpDir, "out")
	testOutputDir := filepath.Join(outDir, "test-outputs")
	if err := os.MkdirAll(testOutputDir, os.ModePerm); err != nil {
		t.Errorf("failed to create test outputs dir: %s", err)
	}
	runSummaryBytes := []byte("{\"schema_id\": \"https://fuchsia.dev/schema/ffx_test/run_summary-8d1dd964.json\"}")
	if err := os.WriteFile(filepath.Join(testOutputDir, runSummaryFilename), runSummaryBytes, os.ModePerm); err != nil {
		t.Errorf("failed to write run_summary.json: %s", err)
	}
	_, err := ffx.Test(ctx, build.TestList{}, outDir)
	assertRunsExpectedCmd(
		err,
		stdout,
		fmt.Sprintf(
			"--target target test run --continue-on-timeout --test-file %s --output-directory %s --show-full-moniker-in-logs",
			filepath.Join(outDir, "test-list.json"), testOutputDir,
		),
	)

	// Snapshot expects a file to be written to tmpDir/snapshotZipName which it will move to tmpDir/new_snapshot.zip.
	if err := os.WriteFile(filepath.Join(tmpDir, snapshotZipName), []byte("snapshot"), os.ModePerm); err != nil {
		t.Errorf("failed to write snapshot")
	}
	assertRunsExpectedCmd(ffx.Snapshot(ctx, tmpDir, "new_snapshot.zip"), stdout, "--target target target snapshot --dir "+tmpDir)
	if _, err := os.Stat(filepath.Join(tmpDir, snapshotZipName)); err == nil {
		t.Errorf("expected snapshot to be renamed")
	}
	if _, err := os.Stat(filepath.Join(tmpDir, "new_snapshot.zip")); err != nil {
		t.Errorf("failed to rename snapshot to new_snapshot.zip: %s", err)
	}

	assertRunsExpectedCmd(ffx.GetConfig(ctx), stdout, "config get")

	assertRunsExpectedCmd(ffx.Run(ctx, "random", "cmd", "with", "args"), stdout, "random cmd with args")

	assertRunsExpectedCmd(ffx.RunWithTarget(ctx, "random", "cmd", "with", "args"), stdout, "--target target random cmd with args")

	assertRunsExpectedCmd(ffx.Stop(), stdout, "daemon stop -t 4000")

	if _, err := ffx.GetSshPrivateKey(ctx); err != nil {
		t.Errorf("failed to get ssh private key: %s", err)
	}

	if _, err := ffx.GetSshAuthorizedKeys(ctx); err != nil {
		t.Errorf("failed to get ssh private key: %s", err)
	}
}

func TestFFXPBArtifacts(t *testing.T) {

	for _, testcase := range []struct {
		name      string
		output    string
		errOutput string
		exitCode  int
		wantPaths []string
		wantError error
	}{
		{
			name:      "OK paths",
			output:    `{"ok": {"paths": [ "pb1.txt", "pb2.txt"]}}`,
			errOutput: "",
			exitCode:  0,
			wantPaths: []string{"pb1.txt", "pb2.txt"},
			wantError: nil,
		},
		{
			name:      "pb not found paths",
			output:    `{"user_error": {"message": "path not found"}}`,
			errOutput: "path not found",
			exitCode:  1,
			wantPaths: []string{},
			wantError: fmt.Errorf("user error: %s", "path not found"),
		},
		{
			name:      "pb not found paths",
			output:    `{"unexpected_error": {"message": "somthing went wrong"}}`,
			errOutput: "exception processing metadata",
			exitCode:  1,
			wantPaths: []string{},
			wantError: fmt.Errorf("unexpected error: somthing went wrong"),
		},
	} {
		t.Run(testcase.name, func(t *testing.T) {
			var testErr error = nil
			if testcase.exitCode != 0 {
				testErr = fmt.Errorf("Exit Code %d: %s", testcase.exitCode, testcase.errOutput)
			}
			paths, err := processPBArtifactsResult(testcase.output, testErr)

			if err != nil {
				if testcase.wantError != nil {
					if err.Error() != testcase.wantError.Error() {
						t.Errorf("Got error %q wanted error: %q", err, testcase.wantError)
					}
				} else if err != nil {
					t.Errorf("Test error: %s", err)
				}
			}

			if len(paths) != len(testcase.wantPaths) {
				t.Errorf("Length mismatch Got  %v want %v", paths, testcase.wantPaths)
			}
			for i := range paths {
				if paths[i] != testcase.wantPaths[i] {
					t.Errorf("mismatch index %d. Got  %v want %v", i, paths, testcase.wantPaths)
				}
			}
		})

	}
}

func TestFFXPBImagePath(t *testing.T) {
	tmpDir := t.TempDir()
	imageDir := filepath.Join(tmpDir, "pb/relpath")
	imagePath := filepath.Join(imageDir, "image")
	err := os.MkdirAll(imageDir, 0777)
	if err != nil {
		t.Errorf("Test error: %q", err)
	}
	err = os.WriteFile(imagePath, []byte("Some bytes to have a size"), 0644)
	if err != nil {
		t.Errorf("Test error: %q", err)
	}

	for _, testcase := range []struct {
		name      string
		output    string
		errOutput string
		exitCode  int
		wantImage *bootserver.Image
		wantError error
	}{
		{
			name:      "OK path",
			output:    `{"ok": {"path": "relpath/image"}}`,
			errOutput: "",
			exitCode:  0,
			wantImage: &bootserver.Image{
				Image: build.Image{Name: "relpath/image", Path: imagePath},
				Size:  25,
			},
			wantError: nil,
		},
		{
			name: "pb not found paths",
			output: `{"user_error": {"message": "path not found"}}
{"type":"unexpected","code":1,"message":"path not found?"}`,
			errOutput: "path not found",
			exitCode:  1,
			wantImage: nil,
			wantError: nil,
		},
		{
			name: "unexpected error",
			output: `{"unexpected_error": {"message": "somthing went wrong"}}
{"type":"unexpected","code":1,"message":"path not found?"}`,
			errOutput: "exception processing metadata",
			exitCode:  1,
			wantImage: nil,
			wantError: nil,
		},
	} {
		t.Run(testcase.name, func(t *testing.T) {
			var testErr error = nil
			if testcase.exitCode != 0 {
				testErr = fmt.Errorf("Exit Code %d: %s", testcase.exitCode, testcase.errOutput)
			}
			image, err := processImageFromPBResult(filepath.Join(tmpDir, "pb"), testcase.output, testErr)
			if err != nil {
				if testcase.wantError != nil {
					if err.Error() != testcase.wantError.Error() {
						t.Errorf("Got error %q wanted error: %q", err, testcase.wantError)
					}
				} else if err != nil {
					t.Errorf("Test error: %s", err)
				}
			}

			if image == nil && testcase.wantImage != nil {
				t.Errorf("Unexpected nil image")
			} else if image != nil && testcase.wantImage == nil {
				t.Errorf("Unexpected non-nil image")
			}
			if image != nil {

				if image.Image.Name != testcase.wantImage.Image.Name {
					t.Errorf("Image name mismatch Got  %v want %v", image.Image, testcase.wantImage.Image)
				}
				if image.Image.Path != testcase.wantImage.Image.Path {
					t.Errorf("Image name mismatch Got  %v want %v", image.Path, testcase.wantImage.Path)
				}
				if image.Size != testcase.wantImage.Size {
					t.Errorf("Image size mismatch Got  %v want %v", image.Size, testcase.wantImage.Size)
				}
			}
		})

	}
}
