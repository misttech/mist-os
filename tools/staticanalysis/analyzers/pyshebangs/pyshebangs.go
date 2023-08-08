// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package pyshebangs

import (
	"context"
	"os"
	"path/filepath"
	"regexp"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/staticanalysis"
)

var (
	// A pattern that matches python shebangs should be flagged.
	pattern = `#!/usr/bin/(?:env\s)?python.*`
	// The paths that needs to be excluded from this check.
	ignorePaths = []string{"build/bazel", "infra", "integration", "vendor", "third_party"}
	// A message that will be included in a comment on the code change.
	message = "Use the Fuchsia vendored Python interpreter in the shebang: `#!/usr/bin/env fuchsia-vendored-python`."
)

type analyzer struct {
	checkoutDir string
}

// New returns an analyzer that checks python shebangs.
func New(checkoutDir string) staticanalysis.Analyzer {
	return &analyzer{checkoutDir: checkoutDir}
}

func (a *analyzer) Analyze(_ context.Context, path string) (findings []*staticanalysis.Finding, err error) {
	if !strings.HasSuffix(path, ".py") {
		return nil, nil
	}
	// check if the path is under the ignored directories.
	for _, ignorePath := range ignorePaths {
		if strings.HasPrefix(path, ignorePath) {
			return nil, nil
		}
	}
	b, err := os.ReadFile(filepath.Join(a.checkoutDir, path))
	if err != nil {
		return nil, err
	}

	firstLine := strings.Split(string(b), "\n")[0]
	match, err := regexp.MatchString(pattern, firstLine)
	if err != nil {
		return nil, err
	}
	if match {
		findings = append(findings, &staticanalysis.Finding{
			Category:  "InvalidPyShebang",
			Message:   message,
			Path:      path,
			StartLine: 1,
			EndLine:   1,
		})
	}
	return findings, nil
}
