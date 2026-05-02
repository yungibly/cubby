package cmd

import (
	"os"

	"github.com/spf13/cobra"
)

var completionCmd = &cobra.Command{
	Use:   "completion [bash|zsh|fish]",
	Short: "Generate shell completion script",
	Long: `Generate a shell completion script for keep.

Bash:
  keep completion bash > /etc/bash_completion.d/keep

Zsh:
  keep completion zsh > "${fpath[1]}/_keep"

Fish:
  keep completion fish > ~/.config/fish/completions/keep.fish
`,
	Args:      cobra.ExactArgs(1),
	ValidArgs: []string{"bash", "zsh", "fish"},
	RunE: func(cmd *cobra.Command, args []string) error {
		switch args[0] {
		case "zsh":
			return rootCmd.GenZshCompletion(os.Stdout)
		case "bash":
			return rootCmd.GenBashCompletion(os.Stdout)
		case "fish":
			return rootCmd.GenFishCompletion(os.Stdout, true)
		}
		return nil
	},
}
