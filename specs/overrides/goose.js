// From goose --help and the help of each subcommand, goose 1.57.0, 2026-11-02.
export default {
  name: "goose",
  subcommands: [
    {name: "configure", description: "Configure goose settings"},
    {
      name: "info",
      description: "Display goose information",
      options: [
        {name: ["-v", "--verbose"], description: "Show verbose information including config.yaml"},
        {name: "--check", description: "Test provider connection and show status"},
      ],
    },
    {name: "doctor", description: "Check that your Goose setup is working"},
    {
      name: "mcp",
      description: "Run one of the mcp servers bundled with goose",
      args: {name: "server"},
    },
    {
      name: "acp",
      description: "Run goose as an ACP agent server on stdio",
      options: [
        {
          name: "--with-builtin",
          description: "Add one or more builtin extensions that are bundled with goose by specifying their names, comma-separated",
          args: {name: "name"},
        },
        {name: "--enable-scheduler", description: "Enable scheduled recipe execution"},
      ],
    },
    {
      name: "serve",
      description: "Start ACP server over HTTP and WebSocket",
      options: [
        {name: "--host", args: {name: "host"}},
        {name: "--port", args: {name: "port"}},
        {name: "--tls", description: "Serve ACP over TLS"},
        {name: "--tls-cert-path", args: {name: "path", template: "filepaths"}},
        {name: "--tls-key-path", args: {name: "path", template: "filepaths"}},
        {name: "--platform", args: {name: "platform", suggestions: ["cli", "desktop"]}},
        {
          name: "--with-builtin",
          description: "Add one or more builtin extensions that are bundled with goose by specifying their names, comma-separated",
          args: {name: "name"},
        },
        {
          name: "--dangerously-unauthenticated",
          description: "Start the ACP endpoint without requiring GOOSE_SERVER__SECRET_KEY",
        },
        {
          name: "--allowed-origin",
          description: "Allow an exact Origin value for ACP CORS",
          args: {name: "origin"},
          isRepeatable: true,
        },
        {name: "--enable-scheduler", description: "Enable scheduled recipe execution"},
      ],
    },
    {
      name: ["session", "s"],
      description: "Start or resume interactive chat sessions",
      subcommands: [
        {
          name: "list",
          description: "List all available sessions",
          options: [
            {
              name: ["-f", "--format"],
              description: "Output format (text, json)",
              args: {name: "format", suggestions: ["text", "json"]},
            },
            {
              name: "--ascending",
              description: "Sort sessions by date in ascending order (oldest first). Default is descending order (newest first).",
            },
            {
              name: ["-w", "--working_dir"],
              description: "Filter sessions by working directory",
              args: {name: "working_dir", template: "folders"},
            },
            {
              name: ["-l", "--limit"],
              description: "Limit the number of results",
              args: {name: "limit"},
            },
          ],
        },
        {
          name: "remove",
          description: "Remove sessions. Runs interactively if no ID, name, or regex is provided.",
          options: [
            {
              name: ["-n", "--name"],
              description: "Specify a name for your chat session",
              args: {name: "name"},
            },
            {
              name: "--session-id",
              description: "Specify a session ID to resume. Requires --resume.",
              args: {name: "session_id"},
            },
            {
              name: "--path",
              description: "Legacy parameter for backward compatibility",
              args: {name: "path", template: "filepaths"},
            },
            {
              name: ["-r", "--regex"],
              description: "Regex for removing matched sessions (optional)",
              args: {name: "regex"},
            },
          ],
        },
        {
          name: "export",
          description: "Export a session",
          options: [
            {
              name: ["-n", "--name"],
              description: "Specify a name for your chat session",
              args: {name: "name"},
            },
            {
              name: "--session-id",
              description: "Specify a session ID to resume. Requires --resume.",
              args: {name: "session_id"},
            },
            {
              name: "--path",
              description: "Legacy parameter for backward compatibility",
              args: {name: "path", template: "filepaths"},
            },
            {
              name: ["-o", "--output"],
              description: "Path to save the exported Markdown. If not provided, output will be sent to stdout",
              args: {name: "output"},
            },
            {
              name: "--format",
              description: "Output format (markdown, json, yaml, html)",
              args: {name: "format", suggestions: ["markdown", "json", "yaml", "html"]},
            },
          ],
        },
        {
          name: "import",
          description: "Import a session from JSON or a Claude Code / Codex / Pi .jsonl",
          args: {
            name: "input",
            description: "Path to a goose session export, or a Claude Code, Codex, or Pi .jsonl transcript",
          },
        },
        {
          name: "diagnostics",
          options: [
            {
              name: ["-n", "--name"],
              description: "Specify a name for your chat session",
              args: {name: "name"},
            },
            {
              name: "--session-id",
              description: "Specify a session ID to resume. Requires --resume.",
              args: {name: "session_id"},
            },
            {
              name: "--path",
              description: "Legacy parameter for backward compatibility",
              args: {name: "path", template: "filepaths"},
            },
            {name: ["-o", "--output"], args: {name: "output"}},
          ],
        },
        {
          name: "rename",
          description: "Rename a session",
          options: [
            {
              name: "--session-id",
              description: "Session ID to rename (e.g., '20250921_143022'). If omitted, prompts interactively.",
              args: {name: "session_id"},
            },
            {
              name: ["-n", "--new-name"],
              description: "New name for the session",
              args: {name: "new_name"},
            },
          ],
        },
      ],
      options: [
        {
          name: ["-n", "--name"],
          description: "Specify a name for your chat session",
          args: {name: "name"},
        },
        {
          name: "--session-id",
          description: "Specify a session ID to resume. Requires --resume.",
          args: {name: "session_id"},
        },
        {
          name: "--path",
          description: "Legacy parameter for backward compatibility",
          args: {name: "path", template: "filepaths"},
        },
        {name: ["-r", "--resume"], description: "Continue from a previous session"},
        {
          name: "--fork",
          description: "Create a new session by copying all messages from a previous session",
        },
        {
          name: "--edit",
          description: "Open the session's conversation in your editor ($VISUAL / $EDITOR / vi) for modification before resuming",
        },
        {name: "--history", description: "Show previous messages when resuming a session"},
        {
          name: "--system",
          description: "Provide additional system instructions to customize the agent's behavior",
          args: {name: "text"},
        },
        {
          name: "--debug",
          description: "When enabled, shows complete tool responses without truncation and full paths.",
        },
        {
          name: "--max-tool-repetitions",
          description: "Set a limit on how many times the same tool can be called consecutively with identical parameters",
          args: {name: "number"},
        },
        {
          name: "--max-turns",
          description: "Set a limit on how many turns (iterations) the agent can take without asking for user input to continue",
          args: {name: "number"},
        },
        {
          name: "--container",
          description: "Run extensions (stdio and built-in) inside the specified container",
          args: {name: "container_id"},
        },
        {
          name: "--with-extension",
          description: "Add stdio extensions from full commands with environment variables",
          args: {name: "command"},
          isRepeatable: true,
        },
        {
          name: "--with-streamable-http-extension",
          description: "Add streamable HTTP extensions from a URL",
          args: {name: "url"},
          isRepeatable: true,
        },
        {
          name: "--with-builtin",
          description: "Add one or more builtin extensions that are bundled with goose by specifying their names, comma-separated",
          args: {name: "name"},
        },
        {
          name: "--no-profile",
          description: "Don't load your default extensions, only use CLI-specified extensions",
        },
        {
          name: "--provider",
          description: "Override the GOOSE_PROVIDER environment variable for this run",
          args: {name: "provider"},
        },
        {
          name: "--model",
          description: "Override the GOOSE_MODEL environment variable for this run",
          args: {name: "model"},
        },
      ],
    },
    {
      name: "run",
      description: "Execute commands from an instruction file or stdin",
      options: [
        {
          name: ["-i", "--instructions"],
          description: "Path to instruction file containing commands. Use - for stdin.",
          args: {name: "file", template: "filepaths"},
        },
        {
          name: ["-t", "--text"],
          description: "Input text containing commands for goose. Use this in lieu of the instructions argument.",
          args: {name: "text"},
        },
        {
          name: "--recipe",
          description: "Recipe name to get recipe file or the full path of the recipe file that defines a custom agent configuration",
          args: {name: "recipe", template: "filepaths"},
        },
        {
          name: "--system",
          description: "Provide additional system instructions to customize the agent's behavior",
          args: {name: "text"},
        },
        {
          name: "--params",
          description: "Key-value parameters to pass to the recipe file. Can be specified multiple times.",
          args: {name: "KEY=VALUE"},
          isRepeatable: true,
        },
        {
          name: "--sub-recipe",
          description: "Specify sub-recipes to include alongside the main recipe",
          args: {name: "recipe"},
          isRepeatable: true,
        },
        {name: "--explain", description: "Show the recipe title, description, and parameters"},
        {name: "--render-recipe", description: "Print the rendered recipe instead of running it."},
        {
          name: ["-n", "--name"],
          description: "Specify a name for your chat session",
          args: {name: "name"},
        },
        {
          name: "--session-id",
          description: "Specify a session ID to resume. Requires --resume.",
          args: {name: "session_id"},
        },
        {
          name: "--path",
          description: "Legacy parameter for backward compatibility",
          args: {name: "path", template: "filepaths"},
        },
        {
          name: ["-s", "--interactive"],
          description: "Continue in interactive mode after processing initial input",
        },
        {
          name: "--no-session",
          description: "Execute commands without creating or using a session file. Useful for automated runs.",
        },
        {
          name: ["-r", "--resume"],
          description: "Continue from a previous run, maintaining the execution state and context.",
        },
        {name: "--stats", description: "Print generation statistics after the run completes"},
        {
          name: "--debug",
          description: "When enabled, shows complete tool responses without truncation and full paths.",
        },
        {
          name: "--max-tool-repetitions",
          description: "Set a limit on how many times the same tool can be called consecutively with identical parameters",
          args: {name: "number"},
        },
        {
          name: "--max-turns",
          description: "Set a limit on how many turns (iterations) the agent can take without asking for user input to continue",
          args: {name: "number"},
        },
        {
          name: "--container",
          description: "Run extensions (stdio and built-in) inside the specified container",
          args: {name: "container_id"},
        },
        {
          name: "--with-extension",
          description: "Add stdio extensions from full commands with environment variables",
          args: {name: "command"},
          isRepeatable: true,
        },
        {
          name: "--with-streamable-http-extension",
          description: "Add streamable HTTP extensions from a URL",
          args: {name: "url"},
          isRepeatable: true,
        },
        {
          name: "--with-builtin",
          description: "Add one or more builtin extensions that are bundled with goose by specifying their names, comma-separated",
          args: {name: "name"},
        },
        {
          name: "--no-profile",
          description: "Don't load your default extensions, only use CLI-specified extensions",
        },
        {
          name: ["-q", "--quiet"],
          description: "Quiet mode. Suppress non-response output, printing only the model response to stdout",
        },
        {
          name: "--output-format",
          description: "Output format (text, json, stream-json)",
          args: {name: "format", suggestions: ["text", "json", "stream-json"]},
        },
        {
          name: "--provider",
          description: "Override the GOOSE_PROVIDER environment variable for this run",
          args: {name: "provider"},
        },
        {
          name: "--model",
          description: "Override the GOOSE_MODEL environment variable for this run",
          args: {name: "model"},
        },
      ],
    },
    {
      name: "recipe",
      description: "Recipe utilities for validation and deeplinking",
      subcommands: [
        {
          name: "validate",
          description: "Validate a recipe",
          args: {
            name: "recipe_name",
            description: "recipe name to get recipe file or full path to the recipe file to validate",
            template: "filepaths",
          },
        },
        {
          name: "deeplink",
          description: "Generate a deeplink for a recipe",
          options: [
            {
              name: ["-p", "--param"],
              description: "Recipe parameter in key=value format (can be specified multiple times)",
              args: {name: "KEY=VALUE"},
              isRepeatable: true,
            },
          ],
          args: {
            name: "recipe_name",
            description: "recipe name to get recipe file or full path to the recipe file to generate deeplink",
            template: "filepaths",
          },
        },
        {
          name: "open",
          description: "Open a recipe in Goose Desktop",
          options: [
            {
              name: ["-p", "--param"],
              description: "Recipe parameter in key=value format (can be specified multiple times)",
              args: {name: "KEY=VALUE"},
              isRepeatable: true,
            },
          ],
          args: {
            name: "recipe_name",
            description: "recipe name or full path to the recipe file",
            template: "filepaths",
          },
        },
        {
          name: "list",
          description: "List available recipes",
          options: [
            {
              name: "--format",
              description: "Output format (text, json)",
              args: {name: "format", suggestions: ["text", "json"]},
            },
            {
              name: ["-v", "--verbose"],
              description: "Show verbose information including recipe descriptions",
            },
          ],
        },
      ],
    },
    {
      name: "skills",
      description: "Skill utilities",
      subcommands: [{name: "list", description: "List all skills available to the goose agent"}],
    },
    {
      name: "plugin",
      description: "Manage plugins",
      subcommands: [
        {
          name: "install",
          description: "Install a plugin from a git repository URL",
          options: [
            {
              name: "--auto-update",
              description: "Automatically update this plugin before plugin skills are loaded",
            },
          ],
          args: {name: "url", description: "URL to a git repository containing a supported plugin"},
        },
        {
          name: "update",
          description: "Update an installed git-backed plugin",
          args: {name: "name", description: "Name of the installed plugin to update"},
        },
      ],
    },
    {
      name: ["schedule", "sched"],
      description: "Manage scheduled jobs",
      subcommands: [
        {
          name: "add",
          description: "Add a new scheduled job",
          options: [
            {
              name: "--schedule-id",
              description: "Unique ID for the recurring scheduled job",
              args: {name: "schedule_id"},
            },
            {
              name: "--cron",
              description: "Cron expression for when to run the job",
              args: {name: "cron"},
            },
            {
              name: "--recipe-source",
              description: "Recipe source (path to file, or base64 encoded recipe string)",
              args: {name: "recipe_source"},
            },
            {
              name: "--params",
              description: "Recipe parameter in KEY=VALUE format (can be specified multiple times)",
              args: {name: "KEY=VALUE"},
              isRepeatable: true,
            },
          ],
        },
        {name: "list", description: "List all scheduled jobs"},
        {
          name: "remove",
          description: "Remove a scheduled job by ID",
          options: [
            {
              name: "--schedule-id",
              description: "ID of the scheduled job to remove (removes the recurring schedule)",
              args: {name: "schedule_id"},
            },
          ],
        },
        {
          name: "sessions",
          description: "List sessions created by a specific schedule",
          options: [
            {name: "--schedule-id", description: "ID of the schedule", args: {name: "schedule_id"}},
            {
              name: ["-l", "--limit"],
              description: "Maximum number of sessions to return",
              args: {name: "limit"},
            },
          ],
        },
        {
          name: "run-now",
          description: "Run a scheduled job immediately",
          options: [
            {
              name: "--schedule-id",
              description: "ID of the schedule to run",
              args: {name: "schedule_id"},
            },
          ],
        },
        {name: "services-status", description: "[Deprecated] Check status of scheduler services"},
        {name: "services-stop", description: "[Deprecated] Stop scheduler services"},
        {name: "cron-help", description: "Show cron expression examples and help"},
      ],
    },
    {
      name: ["gateway", "gw"],
      description: "Manage gateways for external platform integrations",
      subcommands: [
        {name: "status", description: "Show gateway status"},
        {
          name: "start",
          description: "Start a gateway",
          options: [
            {
              name: "--bot-token",
              description: "Authentication token for the gateway platform (e.g., Telegram bot token)",
              args: {name: "bot_token"},
            },
          ],
          args: {name: "gateway_type", description: "Gateway type (e.g., 'telegram')"},
        },
        {
          name: "stop",
          description: "Stop a running gateway",
          args: {name: "gateway_type", description: "Gateway type to stop (e.g., 'telegram')"},
        },
        {
          name: "pair",
          description: "Generate a pairing code for a gateway",
          args: {name: "gateway_type", description: "Gateway type to generate pairing code for"},
        },
      ],
    },
    {
      name: "update",
      description: "Update the goose CLI version",
      options: [
        {
          name: ["-c", "--canary"],
          description: "Update to the latest canary version of the goose CLI, otherwise updates to the latest stable version",
        },
        {name: ["-r", "--reconfigure"], description: "Enforce to re-configure goose during update"},
      ],
    },
    {
      name: "term",
      description: "Terminal-integrated goose session",
      subcommands: [
        {
          name: "init",
          description: "Print shell initialization script",
          options: [
            {
              name: ["-n", "--name"],
              description: "Name for the terminal session",
              args: {name: "name"},
            },
            {
              name: "--default",
              description: "When enabled, anything you type that isn't a valid command will be sent to goose",
            },
            {
              name: "--dangerous-include-history",
              description: "When enabled, goose records recent shell commands and includes them as context when you ask @goose a question",
            },
          ],
          args: {
            name: "shell",
            description: "Shell type (bash, zsh, fish, nu, powershell)",
            suggestions: ["bash", "zsh", "fish", "nu", "powershell"],
          },
        },
        {
          name: "run",
          description: "Run a prompt in the terminal session",
          args: {
            name: "prompt",
            description: "The prompt to send to goose (multiple words allowed without quotes)",
            isVariadic: true,
          },
        },
        {name: "info", description: "Print session info for prompt integration"},
      ],
    },
    {
      name: ["local-models", "lm"],
      description: "Manage local inference models",
      subcommands: [
        {
          name: "search",
          description: "Search HuggingFace for local GGUF and MLX models",
          options: [
            {
              name: ["-l", "--limit"],
              description: "Maximum number of results",
              args: {name: "limit"},
            },
            {
              name: "--repo-prefix",
              description: "Only include repos whose id starts with this prefix",
              args: {name: "repo_prefix"},
            },
            {
              name: "--repo-suffix",
              description: "Only include repos whose id ends with this suffix",
              args: {name: "repo_suffix"},
            },
            {
              name: "--quant",
              description: "Only include variants whose quantization contains this text",
              args: {name: "quant"},
            },
            {
              name: "--ram-gb",
              description: "Override available memory used for recommendations, in GB",
              args: {name: "ram_gb"},
            },
            {name: "--json", description: "Print results as JSON"},
          ],
          args: {name: "query", description: "Search query", isOptional: true},
        },
        {
          name: "download",
          description: "Download a local model from a search result",
          args: {name: "spec", description: "Model spec/download id, e.g. user/repo:Q4_K_M or user/repo"},
        },
        {name: "list", description: "List downloaded local models"},
        {
          name: "delete",
          description: "Delete a downloaded local model",
          args: {name: "id", description: "Model ID to delete"},
        },
      ],
    },
    {
      name: "completion",
      description: "Generate the autocompletion script or Nushell module for the specified shell",
      options: [{name: "--bin-name", description: "Provide a custom binary name", args: {name: "bin_name"}}],
      args: {name: "shell", suggestions: ["bash", "elvish", "fish", "powershell", "nu", "zsh"]},
    },
    {
      name: "review",
      description: "Review the current diff using goose",
      options: [
        {
          name: "--prompt",
          description: "Path to a Markdown file with a custom base review prompt. Replaces the embedded default prompt",
          args: {name: "file", template: "filepaths"},
        },
        {
          name: "--model",
          description: "Default model used for the main review agent and for any check that does not declare its own `model:` in...",
          args: {name: "model"},
        },
        {
          name: "--provider",
          description: "Provider for the main review agent",
          args: {name: "provider"},
        },
        {
          name: "--override-model",
          description: "Force every discovered check to use this model, regardless of the check's own `model:` field",
          args: {name: "model"},
        },
        {
          name: "--turn-limit",
          description: "Default `turn-limit` for orchestrated main-pass subprocesses and for checks that do not declare their own",
          args: {name: "n"},
        },
        {
          name: "--dry-run",
          description: "Print the assembled review prompt and discovered checks instead of running the review",
        },
        {
          name: ["-q", "--quiet"],
          description: "Suppress non-result output from the underlying agent",
        },
        {
          name: "--no-orchestrate",
          description: "Disable the Rust-driven parallel orchestrator and fall back to the single-prompt path that asks the main...",
        },
        {
          name: ["-i", "--instructions"],
          description: "Additional free-form instructions to prepend to the review",
          args: {name: "text"},
        },
        {
          name: ["-f", "--files"],
          description: "Restrict the review to a specific set of files",
          args: {name: "file", isVariadic: true, template: "filepaths"},
        },
        {
          name: ["-c", "--check-filter"],
          description: "Only run checks whose `name` matches one of these",
          args: {name: "name", isVariadic: true},
        },
        {
          name: ["-s", "--check-scope"],
          description: "Alternate directory to search for `.agents/checks/*.md` instead of the repo root",
          args: {name: "dir", template: "folders"},
        },
        {
          name: "--checks-only",
          description: "Skip the main correctness pass and only run check subagents. Mirrors `amp review --checks-only`",
        },
        {
          name: "--summary-only",
          description: "Print only the diff summary; skip the full review. Mirrors `amp review --summary-only`",
        },
        {name: "--severity", description: "Minimum severity to display", args: {name: "level"}},
      ],
      args: {
        name: "range",
        description: "Diff range to review (e.g. \"main...HEAD\"). Defaults to the working tree vs HEAD",
        isOptional: true,
      },
    },
  ],
  options: [
    {name: ["-h", "--help"], description: "Print help", isPersistent: true},
    {name: ["-V", "--version"], description: "Print version"},
  ],
};
