// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package orchestrate

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestFfxEnvContainsSsh(t *testing.T) {
	ffx := &Ffx{Dir: "/foo/bar", bin: "foo/bar/ffx", sslCertPath: "/this/or/something"}
	cmd, err := ffx.Cmd("config", "get")
	if err != nil {
		t.Error(err)
	}
	pathFound := false
	wd, err := os.Getwd()
	if err != nil {
		t.Error(err)
	}
	for _, v := range cmd.Env {
		if strings.HasPrefix(v, "PATH=") && strings.Contains(v, filepath.Join(wd, "openssh-portable", "bin")) {
			pathFound = true
		}
	}
	if !pathFound {
		t.Errorf("SSH not found in env: %v", cmd.Env)
	}
}

func TestFfxSetDefaultTargetNotSet(t *testing.T) {
	// Gets the environment variables used for ffx command invocations, optionally
	// after `Ffx.SetDefaultTarget()` is called (if `defaultTarget != nil`).
	ffxEnv := func(defaultTarget *string) []string {
		ffx := &Ffx{Dir: "/foo/bar", bin: "foo/bar/ffx", sslCertPath: "/this/or/something"}
		ffx.SetDefaultTarget(defaultTarget)
		cmd, err := ffx.Cmd("config", "get")
		if err != nil {
			t.Error(err)
		}
		return cmd.Env
	}

	// Extracts the relevant ffx default target environment variables as a string.
	getDefaultTargetEnvVars := func(env []string) string {
		cmd := exec.Command("bash", "-c", "echo \"<${FUCHSIA_NODENAME+FUCHSIA_NODENAME=}${FUCHSIA_NODENAME},${FUCHSIA_DEVICE_ADDR+FUCHSIA_DEVICE_ADDR=}${FUCHSIA_DEVICE_ADDR}>\"")
		cmd.Env = env
		outputBytes, err := cmd.CombinedOutput()
		if err != nil {
			t.Error(err)
		}
		return strings.TrimSpace(string(outputBytes))
	}

	// Emulate an environment that comes with these env vars defined already.
	os.Setenv("FUCHSIA_NODENAME", "inherited_nodename_value")
	os.Setenv("FUCHSIA_DEVICE_ADDR", "ignored_device_addr_value")

	// Without a default target set, ffx should inherit $FUCHSIA_NODENAME from the
	// current env.
	actual := getDefaultTargetEnvVars(ffxEnv(nil))
	expected := "<FUCHSIA_NODENAME=inherited_nodename_value,>"
	if actual != expected {
		t.Errorf("Output \"%s\" doesn't equal expected \"%s\"", actual, expected)
	}

	// SetDefaultTarget should remove FUCHSIA_DEVICE_ADDR and override
	// FUCHSIA_NODENAME.
	target := "SetDefaultTargetValue"
	actual = getDefaultTargetEnvVars(ffxEnv(&target))
	expected = "<FUCHSIA_NODENAME=SetDefaultTargetValue,>"
	if actual != expected {
		t.Errorf("Output \"%s\" doesn't equal expected \"%s\"", actual, expected)
	}
}
