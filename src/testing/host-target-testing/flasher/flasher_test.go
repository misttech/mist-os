// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package flasher

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/rsa"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"golang.org/x/crypto/ssh"
)

// The easiest way to make a fake key is to just generate a real one.
func generatePublicKey(t *testing.T) ssh.PublicKey {
	privateKey, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	pub, err := ssh.NewPublicKey(&privateKey.PublicKey)
	if err != nil {
		t.Fatal(err)
	}
	return pub
}

// createScript returns the path to a bash script that outputs its name and
// all its arguments.
func createScript(t *testing.T, scriptName string) string {
	contents := "#!/bin/sh\necho \"$0 $@\"\n"
	name := filepath.Join(t.TempDir(), scriptName)
	if err := os.WriteFile(name, []byte(contents), 0o700); err != nil {
		t.Fatal(err)
	}
	return name
}

func createAndRunFfxFlasher(t *testing.T, options ...FfxFlasherOption) string {
	ffxPath := createScript(t, "ffx.sh")
	var output bytes.Buffer
	options = append(options, Stdout(&output))
	flash_manifest := "dir/flash.json"
	ffx, err := ffx.NewFFXTool(ffxPath)
	if err != nil {
		t.Fatal(err)
	}
	flasher, err := NewFfxFlasher(ffx, flash_manifest, false, options...)
	if err != nil {
		t.Fatal(err)
	}
	if err := flasher.Flash(context.Background()); err != nil {
		t.Fatal(err)
	}
	result := strings.ReplaceAll(output.String(), ffxPath, "ffx")
	return result
}

func createAndRunScriptFlasher(t *testing.T, options ...ScriptFlasherOption) string {
	flashScriptPath := createScript(t, "flash.sh")
	var output bytes.Buffer
	options = append(options, ScriptFlasherStdout(&output))
	flash_manifest := "dir/flash.json"
	flasher, err := NewScriptFlasher(flashScriptPath, flash_manifest, options...)
	if err != nil {
		t.Fatal(err)
	}
	if err := flasher.Flash(context.Background()); err != nil {
		t.Fatal(err)
	}
	result := strings.ReplaceAll(output.String(), flashScriptPath, "flash.sh")
	return result
}

func TestDefault(t *testing.T) {
	result := strings.Trim(createAndRunFfxFlasher(t), "\n")
	expected_result := "target flash --manifest dir/flash.json"
	if !strings.HasPrefix(result, "ffx") || !strings.HasSuffix(result, expected_result) {
		t.Fatalf("target flash result mismatched: " + result)
	}
}

func TestSSHKeys(t *testing.T) {
	sshKey := generatePublicKey(t)
	result := strings.Trim(createAndRunFfxFlasher(t, SSHPublicKey(sshKey)), "\n")
	segs := strings.Fields(result)
	result = strings.Join(segs[:len(segs)-3], " ")
	expected_result := "target flash --authorized-keys"
	if !strings.HasPrefix(result, "ffx") || !strings.HasSuffix(result, expected_result) {
		t.Fatalf("target flash result mismatched: " + result)
	}
}

func TestScriptFlasherSSHKeys(t *testing.T) {
	sshKey := generatePublicKey(t)
	result := strings.Trim(createAndRunScriptFlasher(t, ScriptFlasherSSHPublicKey(sshKey)), "\n")
	segs := strings.Fields(result)
	result = strings.Join(segs[:len(segs)-1], " ")
	expected_result := "--ssh-key"
	if !strings.HasPrefix(result, "flash.sh") || !strings.HasSuffix(result, expected_result) {
		t.Fatalf("target flash result mismatched: " + result)
	}
}
