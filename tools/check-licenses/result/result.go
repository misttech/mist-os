// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package result

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"os"
	"path"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/directory"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/file"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project"
)

const (
	indent = "  "
)

// SaveResults saves the results to the output files defined in the config file.
func SaveResults(cmdConfig interface{}, cmdMetrics MetricsInterface) (string, error) {
	var b strings.Builder

	s, err := savePackageInfo("cmd", cmdConfig, cmdMetrics)
	if err != nil {
		return "", err
	}
	b.WriteString(s)

	s, err = savePackageInfo("project", project.Config, project.Metrics)
	if err != nil {
		return "", err
	}
	b.WriteString(s)

	s, err = savePackageInfo("file", file.Config, file.Metrics)
	if err != nil {
		return "", err
	}
	b.WriteString(s)

	s, err = savePackageInfo("directory", directory.Config, directory.Metrics)
	if err != nil {
		return "", err
	}
	b.WriteString(s)

	s, err = savePackageInfo("result", Config, Metrics)
	if err != nil {
		return "", err
	}
	b.WriteString(s)

	if Config.RunAnalysis {
		err = RunChecks()
		if err != nil {
			return "", err
		}
	} else {
		log.Printf(" -> Not running tests on results.\n")
	}

	if Config.OutputLicenseFile {
		s1, err := expandTemplates()
		if err != nil {
			return "", err
		}
		b.WriteString(s1)
	} else {
		log.Printf(" -> Not expanding templates.\n")
	}

	projectList := make([]*project.Project, 0)
	for _, p := range project.FilteredProjects {
		projectList = append(projectList, p)
	}
	sort.Sort(project.Order(projectList))

	if Config.OutputLicenseFile {
		s, err := generateSPDXDoc(
			Config.SPDXDocName,
			projectList,
			project.RootProject,
		)
		if err != nil {
			// TODO(fxbug.dev/116788): Return ("", err) instead once SPDX generation
			// stops flaking. For now, do not treat a failure from generateSPDXDoc
			// as fatal.
			b.WriteString(fmt.Sprintf("SPDX doc generation failed: %v\n", err))
		} else {
			b.WriteString(s)
		}
	} else {
		log.Printf(" -> Not generating SPDX doc.\n")
	}

	if err = writeFile("summary", []byte(b.String())); err != nil {
		return "", err
	}

	b.WriteString("\n")
	if Config.OutDir != "" {
		b.WriteString(fmt.Sprintf("Full summary and output files -> %s\n", Config.OutDir))
	} else {
		b.WriteString("Set the 'outputdir' arg in the config file to save detailed information to disk.\n")
	}

	if err := compressTarGZ(Config.OutDir, path.Join(Config.RootOutDir, "runFiles")); err != nil {
		return "", err
	}

	return b.String(), nil
}

// This retrieves all the relevant metrics information for a given package.
// e.g. the //tools/check-licenses/directory package.
func savePackageInfo(pkgName string, c interface{}, m MetricsInterface) (string, error) {
	var b strings.Builder

	fmt.Fprintf(&b, "\n%s Metrics:\n", strings.Title(pkgName))

	counts := m.Counts()
	keys := make([]string, 0, len(counts))
	for k := range counts {
		keys = append(keys, k)
	}
	sort.Strings(keys)

	for _, k := range keys {
		fmt.Fprintf(&b, "%s%s: %s\n", indent, k, strconv.Itoa(counts[k]))
	}
	if Config.OutDir != "" {
		if _, err := os.Stat(Config.OutDir); os.IsNotExist(err) {
			err := os.MkdirAll(Config.OutDir, 0755)
			if err != nil {
				return "", fmt.Errorf("failed to make directory %s: %w", Config.OutDir, err)
			}
		}

		if err := saveMetrics(pkgName, m); err != nil {
			return "", err
		}
	}
	return b.String(), nil
}

// Save the "Files" and "Values" metrics: freeform data stored in a map with string keys.
func saveMetrics(pkg string, m MetricsInterface) error {
	for k, bytes := range m.Files() {

		// Spaces and commas are not allowed in file or folder names.
		// Replace spaces and commas with underscores.
		k = strings.Replace(k, " ", "_", -1)
		k = strings.Replace(k, ",", "_", -1)

		path := filepath.Join(pkg, k)

		// For uploading to GCS, the license files need to be saved in
		// a separate directory.
		if Config.LicenseOutDir != "" && strings.HasPrefix(path, "license/matches/") {
			path = strings.TrimPrefix(path, "license/matches/")
			if err := writeFileRoot(path, bytes, Config.LicenseOutDir); err != nil {
				return fmt.Errorf("failed to write Files file %s: %w", path, err)
			}
		} else {
			if err := writeFile(path, bytes); err != nil {
				return fmt.Errorf("failed to write Files file %s: %w", path, err)
			}
		}
	}

	for k, v := range m.Values() {
		sort.Strings(v)
		if bytes, err := json.MarshalIndent(v, "", "  "); err != nil {
			return fmt.Errorf("failed to marshal indent for key %s: %w", k, err)
		} else {
			k = strings.Replace(k, " ", "_", -1)
			path := filepath.Join(pkg, k)
			if err := writeFile(path, bytes); err != nil {
				return fmt.Errorf("failed to write Values file %s: %w", path, err)
			}
		}
	}
	return nil
}

// Write file to <Config.OutDir>/<path parameter>
func writeFile(path string, data []byte) error {
	return writeFileRoot(path, data, Config.OutDir)
}

// Write file to <root parameter>/<path parameter>
func writeFileRoot(path string, data []byte, root string) error {
	path = filepath.Join(root, path)
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return fmt.Errorf("failed to make directory %s: %w", dir, err)
	}
	if err := os.WriteFile(path, data, 0666); err != nil {
		return fmt.Errorf("failed to write file %s: %w", path, err)
	}
	return nil
}

func compressGZ(path string) error {
	d, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("failed to read file %s: %w", path, err)
	}

	buf := bytes.Buffer{}
	zw := gzip.NewWriter(&buf)
	if _, err := zw.Write(d); err != nil {
		return fmt.Errorf("failed to write zipped file %w", err)
	}
	if err := zw.Close(); err != nil {
		return fmt.Errorf("failed to close zipped file %w", err)
	}
	path, err = filepath.Rel(Config.OutDir, path)
	if err != nil {
		return err
	}
	return writeFile(path+".gz", buf.Bytes())
}

func compressTarGZ(root string, out string) error {
	buf := bytes.Buffer{}
	zw := gzip.NewWriter(&buf)
	tw := tar.NewWriter(zw)

	filepath.Walk(root, func(path string, info os.FileInfo, err error) error {
		var header *tar.Header

		if header, err = tar.FileInfoHeader(info, path); err != nil {
			return err
		}

		header.Name, _ = filepath.Rel(Config.OutDir, path)
		if err = tw.WriteHeader(header); err != nil {
			return err
		}

		if !info.IsDir() {
			content, err := os.Open(path)
			if err != nil {
				return err
			}

			if _, err := io.Copy(tw, content); err != nil {
				return err
			}
		}

		return nil
	})

	if err := tw.Close(); err != nil {
		return err
	}
	if err := zw.Close(); err != nil {
		return err
	}
	return writeFileRoot(out+".tar.gz", buf.Bytes(), "")
}
