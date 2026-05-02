package cmd

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"
)

var diffCmd = &cobra.Command{
	Use:   "diff [file|dir]...",
	Short: "Show differences between live files and stored versions",
	Long: `Show a line-by-line diff between each live file and its stored version.
For directories, all differing files are shown. With no arguments, diffs
everything in the storage directory that differs from live.
Paths are resolved relative to your current working directory.`,
	RunE: runDiff,
	ValidArgsFunction: func(cmd *cobra.Command, args []string, toComplete string) ([]string, cobra.ShellCompDirective) {
		return nil, cobra.ShellCompDirectiveDefault
	},
}

func init() {
	rootCmd.AddCommand(diffCmd)
}

func diffPair(liveFile, storeFile, label string) string {
	liveHash, err1 := hashFile(liveFile)
	storeHash, err2 := hashFile(storeFile)
	if err1 == nil && err2 == nil && string(liveHash) == string(storeHash) {
		return ""
	}

	var buf bytes.Buffer
	buf.WriteString(fmt.Sprintf("\n%s  %s\n", yellow("~"), bold(label)))
	buf.WriteString(fmt.Sprintf("%s\n", dim(strings.Repeat("─", 60))))

	cmd := exec.Command("diff",
		"--color=always",
		"--unified=3",
		"--label", "stored",
		"--label", "live",
		storeFile, liveFile,
	)
	var out bytes.Buffer
	cmd.Stdout = &out
	cmd.Stderr = os.Stderr
	cmd.Run()
	buf.Write(out.Bytes())
	return buf.String()
}

func outputWithPager(output string) {
	lines := strings.Count(output, "\n")
	rows := termRows()

	if lines <= rows {
		fmt.Print(output)
		return
	}

	pager := exec.Command("less", "-R")
	pager.Stdin = strings.NewReader(output)
	pager.Stdout = os.Stdout
	pager.Stderr = os.Stderr
	if err := pager.Run(); err != nil {
		fmt.Print(output)
	}
}

func termRows() int {
	cmd := exec.Command("tput", "lines")
	cmd.Stdin = os.Stdin
	out, err := cmd.Output()
	if err != nil {
		return 40
	}
	var rows int
	fmt.Sscanf(strings.TrimSpace(string(out)), "%d", &rows)
	if rows <= 0 {
		return 40
	}
	return rows
}

// diffTarget returns diff output for a single target, empty string if identical
func diffTarget(abs, storePath, rel string) string {
	var buf bytes.Buffer

	liveInfo, liveErr := os.Stat(abs)
	if os.IsNotExist(liveErr) {
		buf.WriteString(fmt.Sprintf("%s  live path not found: ~/%s\n", red("✗"), rel))
		return buf.String()
	}
	if _, err := os.Stat(storePath); os.IsNotExist(err) {
		buf.WriteString(fmt.Sprintf("%s  not in storage: %s\n", red("✗"), rel))
		return buf.String()
	}

	if liveInfo.IsDir() {
		// files in store — check against live
		filepath.Walk(storePath, func(sp string, info os.FileInfo, err error) error {
			if err != nil || info.IsDir() {
				return nil
			}
			fileRel, _ := filepath.Rel(storePath, sp)
			liveFile := filepath.Join(abs, fileRel)
			displayLabel := "~/" + rel + "/" + fileRel

			if _, err := os.Stat(liveFile); os.IsNotExist(err) {
				buf.WriteString(fmt.Sprintf("\n%s  %s  %s\n", red("-"), bold(displayLabel), dim("(missing from live)")))
				return nil
			}
			buf.WriteString(diffPair(liveFile, sp, displayLabel))
			return nil
		})

		// files in live not in store
		filepath.Walk(abs, func(lp string, info os.FileInfo, err error) error {
			if err != nil || info.IsDir() {
				return nil
			}
			fileRel, _ := filepath.Rel(abs, lp)
			sp := filepath.Join(storePath, fileRel)
			displayLabel := "~/" + rel + "/" + fileRel
			if _, err := os.Stat(sp); os.IsNotExist(err) {
				buf.WriteString(fmt.Sprintf("\n%s  %s  %s\n", green("+"), bold(displayLabel), dim("(not in storage)")))
			}
			return nil
		})
	} else {
		buf.WriteString(diffPair(abs, storePath, "~/"+rel))
	}

	return buf.String()
}

func runDiff(cmd *cobra.Command, args []string) error {
	home, _ := os.UserHomeDir()
	store := resolvedStoreDir()

	var output strings.Builder

	if len(args) == 0 {
		if _, err := os.Stat(store); os.IsNotExist(err) {
			errorf("storage directory not found: %s", store)
			return nil
		}
		filepath.Walk(store, func(storePath string, info os.FileInfo, err error) error {
			if err != nil {
				return nil
			}
			rel, _ := filepath.Rel(store, storePath)
			if rel == "." || isIgnored(rel) {
				if info.IsDir() && rel != "." {
					return filepath.SkipDir
				}
				return nil
			}
			if info.IsDir() {
				return nil
			}
			livePath := filepath.Join(home, rel)
			output.WriteString(diffTarget(livePath, storePath, rel))
			return nil
		})
	} else {
		for _, target := range args {
			abs, err := filepath.Abs(target)
			if err != nil {
				errorf("cannot resolve %s: %v", target, err)
				continue
			}
			if !strings.HasPrefix(abs, home+string(filepath.Separator)) {
				errorf("%s is outside $HOME", abs)
				continue
			}
			rel := strings.TrimPrefix(abs, home+string(filepath.Separator))
			storePath := filepath.Join(store, rel)
			output.WriteString(diffTarget(abs, storePath, rel))
		}
	}

	result := output.String()
	if strings.TrimSpace(result) == "" {
		fmt.Printf("%s  %s\n", green("="), dim("everything identical"))
		return nil
	}
	outputWithPager(result)
	return nil
}
