package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"
)

var diffCmd = &cobra.Command{
	Use:   "diff <file>...",
	Short: "Show differences between live files and stored versions",
	Long: `Show a line-by-line diff between each live file and its stored version.
Paths are resolved relative to your current working directory.`,
	Args: cobra.MinimumNArgs(1),
	RunE: runDiff,
	ValidArgsFunction: func(cmd *cobra.Command, args []string, toComplete string) ([]string, cobra.ShellCompDirective) {
		return nil, cobra.ShellCompDirectiveDefault
	},
}

func init() {
	rootCmd.AddCommand(diffCmd)
}

func runDiff(cmd *cobra.Command, args []string) error {
	home, _ := os.UserHomeDir()
	store := resolvedStoreDir()
	cwd, _ := os.Getwd()

	for _, target := range args {
		abs, err := filepath.Abs(filepath.Join(cwd, target))
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

		// check both sides exist
		if _, err := os.Stat(abs); os.IsNotExist(err) {
			errorf("live file not found: ~/%s", rel)
			continue
		}
		if _, err := os.Stat(storePath); os.IsNotExist(err) {
			errorf("not in storage: %s", rel)
			continue
		}

		// check if identical first
		liveHash, err1 := hashFile(abs)
		storeHash, err2 := hashFile(storePath)
		if err1 == nil && err2 == nil && string(liveHash) == string(storeHash) {
			fmt.Printf("%s  %s  %s\n", green("="), bold("~/"+rel), dim("(identical)"))
			continue
		}

		// print header
		fmt.Printf("\n%s  %s\n", yellow("~"), bold("~/"+rel))
		fmt.Printf("%s\n", dim(strings.Repeat("─", 60)))

		// run diff with color if supported
		diffCmd := exec.Command("diff",
			"--color=always",
			"--unified=3",
			"--label", "stored",
			"--label", "live",
			storePath, abs,
		)
		diffCmd.Stdout = os.Stdout
		diffCmd.Stderr = os.Stderr
		diffCmd.Run() // diff exits 1 when files differ, which is expected — ignore error
	}

	return nil
}
