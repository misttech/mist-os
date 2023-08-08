// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package pyshebangs

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"

	"go.fuchsia.dev/fuchsia/tools/staticanalysis"
)

type file struct {
	path    string
	content string
}

func TestAnalyzer(t *testing.T) {
	tests := []struct {
		name     string
		path     string
		files    []file
		expected []*staticanalysis.Finding
	}{
		{
			name: "file with fuchsia vendored python shebang",
			path: "src/test.py",
			files: []file{{
				path:    "src/test.py",
				content: `#!/usr/bin/env fuchsia-vendored-python`,
			}},
		},
		{
			name: "file with python shebang",
			path: "src/test.py",
			files: []file{{
				path:    "src/test.py",
				content: `#!/usr/bin/env python`,
			}},
			expected: []*staticanalysis.Finding{
				{
					Category:  "InvalidPyShebang",
					Message:   message,
					Path:      "src/test.py",
					StartLine: 1,
					EndLine:   1,
				},
			},
		},
		{
			name: "file with python3.x shebang",
			path: "src/test.py",
			files: []file{{
				path:    "src/test.py",
				content: `#!/usr/bin/env python3.11`,
			}},
			expected: []*staticanalysis.Finding{
				{
					Category:  "InvalidPyShebang",
					Message:   message,
					Path:      "src/test.py",
					StartLine: 1,
					EndLine:   1,
				},
			},
		},
		{
			name: "excluded directory file with python shebang",
			path: "build/bazel-sdk/test.py",
			files: []file{{
				path:    "build/bazel-sdk/test.py",
				content: `#!/usr/bin/env python`,
			}},
		},
		{
			name: "excluded directory file with python shebang",
			path: "build/bazel-sdk/test.py",
			files: []file{{
				path:    "build/bazel-sdk/test.py",
				content: `#!/usr/bin/env python`,
			}},
		},
		{
			name: "non python file",
			path: "test.go",
			files: []file{{
				path:    "test.go",
				content: `#!/usr/bin/env python`,
			}},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			checkoutDir := t.TempDir()
			analyzer := analyzer{checkoutDir: checkoutDir}
			for _, file := range test.files {
				dirPath := filepath.Join(checkoutDir, filepath.Dir(test.path))
				if err := os.MkdirAll(dirPath, 0o700); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(filepath.Join(checkoutDir, file.path), []byte(file.content), 0o600); err != nil {
					t.Fatal(err)
				}
			}
			findings, err := analyzer.Analyze(context.Background(), test.path)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(test.expected, findings, cmpopts.EquateEmpty()); diff != "" {
				t.Errorf("Analyzer diff (-want +got):\n%s", diff)
			}
		})
	}
}
