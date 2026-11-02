// From claude --help and the help of each subcommand, Claude Code 2.1.316, 2026-11-02.
export default {
  name: "claude",
  subcommands: [
    {
      name: "agents",
      description: "Manage background agents",
      options: [
        {
          name: "--add-dir",
          description: "Additional directory to allow tool access to in dispatched sessions (repeatable)",
          args: {name: "directory", template: "folders"},
          isRepeatable: true,
        },
        {
          name: "--agent",
          description: "Default agent for sessions dispatched from agent view. Overrides the 'agent' setting.",
          args: {name: "agent"},
        },
        {name: "--all", description: "With --json: also include completed background sessions"},
        {
          name: "--allow-dangerously-skip-permissions",
          description: "Make bypass-permissions mode available to dispatched sessions without defaulting to it",
        },
        {
          name: "--cwd",
          description: "Show only background sessions started under <path>",
          args: {name: "path", template: "filepaths"},
        },
        {
          name: "--dangerously-skip-permissions",
          description: "Alias for --permission-mode bypassPermissions",
        },
        {
          name: "--effort",
          description: "Default effort level for sessions dispatched from agent view",
          args: {name: "level"},
        },
        {
          name: "--json",
          description: "Print active sessions (interactive and background) as a JSON array and exit (for scripting",
        },
        {
          name: "--mcp-config",
          description: "MCP server configuration to apply to dispatched sessions (repeatable)",
          args: {name: "config"},
          isRepeatable: true,
        },
        {
          name: "--model",
          description: "Default model for sessions dispatched from agent view",
          args: {name: "model"},
        },
        {
          name: "--permission-mode",
          description: "Default permission mode for sessions dispatched from agent view",
          args: {name: "mode"},
        },
        {
          name: "--plugin-dir",
          description: "Load plugins from specified directory for the agent view and dispatched sessions",
          args: {name: "path", template: "filepaths"},
          isRepeatable: true,
        },
        {name: "--restricted", description: "Start dispatched sessions in restricted mode"},
        {
          name: "--setting-sources",
          description: "Comma-separated list of setting sources to load (user, project, local).",
          args: {name: "sources"},
        },
        {
          name: "--settings",
          description: "Settings file or JSON string to apply to the agent view and dispatched sessions",
          args: {name: "file-or-json"},
        },
        {
          name: "--strict-mcp-config",
          description: "Only use MCP servers from --mcp-config in dispatched sessions",
        },
      ],
    },
    {
      name: "attach",
      description: "Open a background session in this terminal. <id> is the short id that `claude --bg` prints and `claude...",
      args: {name: "id|name"},
    },
    {
      name: "auth",
      description: "Manage authentication",
      subcommands: [
        {
          name: "login",
          description: "Sign in to your Anthropic account",
          options: [
            {name: "--claudeai", description: "Use Claude subscription (default)"},
            {
              name: "--console",
              description: "Use Anthropic Console (API usage billing) instead of Claude subscription",
            },
            {
              name: "--email",
              description: "Pre-populate email address on the login page",
              args: {name: "email"},
            },
            {name: "--sso", description: "Force SSO login flow"},
          ],
        },
        {name: "logout", description: "Log out from your Anthropic account"},
        {
          name: "status",
          description: "Show authentication status",
          options: [
            {name: "--json", description: "Output as JSON (default)"},
            {name: "--text", description: "Output as human-readable text"},
          ],
        },
      ],
    },
    {
      name: "auto-mode",
      description: "Inspect or reset auto mode configuration",
      subcommands: [
        {
          name: "config",
          description: "Print the effective auto mode config as JSON: your settings where set, defaults otherwise",
        },
        {
          name: "critique",
          description: "Get AI feedback on your custom auto mode rules",
          options: [{name: "--model", description: "Override which model is used", args: {name: "model"}}],
        },
        {
          name: "defaults",
          description: "Print the default auto mode environment, allow, soft_deny, and hard_deny rules as JSON",
          options: [
            {
              name: "--label",
              description: "Show only rules whose label starts with this prefix (case-insensitive)",
              args: {name: "prefix"},
            },
          ],
        },
        {
          name: "reset",
          description: "Reset auto mode configuration to the shipped defaults by removing the autoMode section from your user...",
          options: [{name: ["-y", "--yes"], description: "Skip the confirmation prompt"}],
        },
      ],
    },
    {name: "doctor", description: "Check the health of your Claude Code installation"},
    {
      name: "gateway",
      description: "Run the enterprise auth/telemetry gateway",
      options: [
        {
          name: "--config",
          description: "Path to gateway YAML config",
          args: {name: "path", template: "filepaths"},
        },
      ],
    },
    {
      name: "import",
      description: "Import config from another AI coding agent into Claude Code",
      options: [
        {name: "--dry-run", description: "Show what would be imported without writing anything"},
        {
          name: "--yes",
          description: "Skip the interactive picker. On headless surfaces, pass --yes=<digest> from the `/import` preview.",
        },
      ],
      args: {
        name: "source",
        description: "Which agent to import from (codex, gemini, cursor)",
        isOptional: true,
        suggestions: ["codex", "gemini", "cursor"],
      },
    },
    {
      name: "install",
      description: "Install Claude Code native build",
      options: [{name: "--force", description: "Force installation even if already installed"}],
      args: {name: "target", isOptional: true, suggestions: ["stable", "latest"]},
    },
    {
      name: "logs",
      description: "Print a background session's recent terminal output",
      args: {name: "id|name"},
    },
    {
      name: "mcp",
      description: "Configure and manage MCP servers",
      subcommands: [
        {
          name: "add",
          description: "Add an MCP server to Claude Code.",
          options: [
            {
              name: "--callback-port",
              description: "Fixed port for OAuth callback (for servers requiring pre-registered redirect URIs)",
              args: {name: "port"},
            },
            {
              name: "--client-id",
              description: "OAuth client ID for HTTP/SSE servers",
              args: {name: "clientId"},
            },
            {
              name: "--client-secret",
              description: "Prompt for OAuth client secret (or set MCP_CLIENT_SECRET env var)",
            },
            {
              name: ["-e", "--env"],
              description: "Set environment variables (e.g. -e KEY=value)",
              args: {name: "env", isVariadic: true},
            },
            {
              name: ["-H", "--header"],
              description: "Set headers for HTTP/SSE servers (e.g. -H \"X-Api-Key: abc123\" -H \"X-Custom: value\")",
              args: {name: "header", isVariadic: true},
            },
            {
              name: ["-s", "--scope"],
              description: "Configuration scope (local, user, or project) (default: \"local\")",
              args: {name: "scope", suggestions: ["local", "user", "project"]},
            },
            {
              name: ["-t", "--transport"],
              description: "Transport type (stdio, sse, http). Defaults to stdio if not specified.",
              args: {name: "transport", suggestions: ["stdio", "sse", "http"]},
            },
          ],
          args: [
            {name: "name"},
            {name: "commandOrUrl"},
            {name: "args", isOptional: true, isVariadic: true},
          ],
        },
        {
          name: "add-from-claude-desktop",
          description: "Import MCP servers from Claude Desktop (Mac and WSL only)",
          options: [
            {
              name: ["-s", "--scope"],
              description: "Configuration scope (local, user, or project) (default: \"local\")",
              args: {name: "scope", suggestions: ["local", "user", "project"]},
            },
          ],
        },
        {
          name: "add-json",
          description: "Add an MCP server (stdio, SSE, HTTP, or WebSocket) with a JSON string",
          options: [
            {
              name: "--client-secret",
              description: "Prompt for OAuth client secret (or set MCP_CLIENT_SECRET env var)",
            },
            {
              name: ["-s", "--scope"],
              description: "Configuration scope (local, user, or project) (default: \"local\")",
              args: {name: "scope", suggestions: ["local", "user", "project"]},
            },
          ],
          args: [{name: "name"}, {name: "json"}],
        },
        {name: "get", description: "Get details about an MCP server", args: {name: "name"}},
        {name: "list", description: "List configured MCP servers"},
        {
          name: "login",
          description: "Authenticate with an MCP server (HTTP, SSE, or claude.ai connector)",
          options: [
            {
              name: "--no-browser",
              description: "Print the authorization URL instead of opening a browser",
            },
          ],
          args: {name: "name"},
        },
        {
          name: "logout",
          description: "Clear stored OAuth credentials for an MCP server",
          args: {name: "name"},
        },
        {
          name: "remove",
          description: "Remove an MCP server",
          options: [
            {
              name: ["-s", "--scope"],
              description: "Configuration scope (local, user, or project) - if not specified, removes from whichever scope it exists in",
              args: {name: "scope", suggestions: ["local", "user", "project"]},
            },
          ],
          args: {name: "name"},
        },
        {
          name: "reset-project-choices",
          description: "Reset all approved and rejected project-scoped (.mcp.json) servers within this project",
        },
        {
          name: "serve",
          description: "Start the Claude Code MCP server",
          options: [
            {name: ["-d", "--debug"], description: "Enable debug mode"},
            {name: "--verbose", description: "Override verbose mode setting from config"},
          ],
        },
      ],
    },
    {
      name: ["plugin", "plugins"],
      description: "Manage Claude Code plugins",
      subcommands: [
        {
          name: "configure",
          description: "Show a plugin's options and which are unset, or save values from stdin with --values-stdin",
          options: [
            {name: "--json", description: "Output as JSON"},
            {
              name: "--values-stdin",
              description: "Read option values from stdin as a JSON object of single-line strings; options left out keep their values",
            },
          ],
          args: {name: "plugin"},
        },
        {
          name: "details",
          description: "Show a plugin's component inventory and projected token cost",
          args: {name: "name"},
        },
        {
          name: "disable",
          description: "Disable an enabled plugin",
          options: [
            {name: ["-a", "--all"], description: "Disable all enabled plugins"},
            {
              name: "--json",
              description: "Print one machine-readable result line on stdout instead of the human message (same exit codes)",
            },
            {
              name: ["-s", "--scope"],
              description: "Installation scope: user, project, local (default: auto-detect)",
              args: {name: "scope", suggestions: ["user", "project", "local"]},
            },
          ],
          args: {name: "plugin", isOptional: true},
        },
        {
          name: "enable",
          description: "Enable a disabled plugin",
          options: [
            {
              name: "--json",
              description: "Print one machine-readable result line on stdout instead of the human message (same exit codes)",
            },
            {
              name: ["-s", "--scope"],
              description: "Installation scope: user, project, local (default: auto-detect)",
              args: {name: "scope", suggestions: ["user", "project", "local"]},
            },
          ],
          args: {name: "plugin"},
        },
        {
          name: "eval",
          description: "Run eval cases (<eval dir>/**/case.yaml or prompt.md + graders/*.md",
          subcommands: [
            {
              name: "init",
              description: "Author an eval suite under the eval dir",
              options: [
                {
                  name: "--bare",
                  description: "Write a blank template (prompt.md + graders/criteria.md) instead of running the interview",
                },
                {
                  name: "--eval-dir",
                  description: "Directory (below the current directory) to write cases into (default",
                  args: {name: "dir", template: "folders"},
                },
                {
                  name: ["-i", "--interactive"],
                  description: "Run the authoring interview (already the default in a terminal); requires an interactive terminal",
                },
              ],
              args: {name: "name", isOptional: true},
            },
          ],
          options: [
            {
              name: "--ablation",
              description: "Run a no-plugin baseline arm and report the score delta (none | with-without; default",
              args: {name: "mode", suggestions: ["none", "with-without"]},
            },
            {
              name: "--allow-real-servers",
              description: "With --mocks record: also start the plugin's REAL MCP server processes for servers that have no mock",
            },
            {
              name: "--allow-tools",
              description: "Operator grant for gated tools (Bash, Write, Edit, WebFetch, mcp__*)",
              args: {name: "tools", isVariadic: true},
            },
            {name: "--case", description: "Filter cases by name glob", args: {name: "glob"}},
            {
              name: ["-j", "--concurrency"],
              description: "Run up to <n> agent runs at once (1-8; default 1)",
              args: {name: "n"},
            },
            {
              name: "--disallow-tools",
              description: "Built-in tools to keep from Claude in every case, even if granted (e.g. Agent TaskStop)",
              args: {name: "tools", isVariadic: true},
            },
            {
              name: "--eval-dir",
              description: "Directory name (below the plugin) that holds the eval cases",
              args: {name: "dir", template: "folders"},
            },
            {
              name: "--json",
              description: "Print the full run result (prompts, graders, per-run scores) as JSON to stdout, or write it to this .json file",
              args: {name: "path", isOptional: true, template: "filepaths"},
            },
            {
              name: "--judge-model",
              description: "Override LLM-grader model (default: haiku)",
              args: {name: "model"},
            },
            {name: "--keep-temp", description: "Preserve scaffold dirs for debugging"},
            {
              name: "--max-cost-usd",
              description: "Optional hard cost ceiling; abort and report partial results if hit (exit 2)",
              args: {name: "usd"},
            },
            {
              name: "--mocks",
              description: "Mock stand-ins for MCP servers, from <eval dir>/mocks/ (record | off; default",
              args: {name: "mode", suggestions: ["record", "off"]},
            },
            {name: "--model", description: "Override model for all cases", args: {name: "model"}},
            {
              name: "--no-publish",
              description: "Keep the HTML report local only; skip publishing it to claude.ai",
            },
            {name: "--no-scaffold", description: "Explicitly skip scaffold_script"},
            {
              name: "--output-dir",
              description: "Directory for aggregate-result.json (default: ./<eval dir>/results/<timestamp>/)",
              args: {name: "dir", template: "folders"},
            },
            {
              name: "--publish-report",
              description: "Also require publishing the report to claude.ai (already the default when your account supports it)",
            },
            {
              name: "--report",
              description: "Write the self-contained HTML report (scores, prompts, grader verdicts) to <path> instead of the results dir",
              args: {name: "path", template: "filepaths"},
            },
            {
              name: "--runs",
              description: "Override per-case runs (default: case.runs ?? 3)",
              args: {name: "n"},
            },
            {
              name: "--scaffold",
              description: "Run each case's scaffold_script (runs author-supplied bash as you",
            },
            {
              name: "--tag",
              description: "Filter cases by tag (repeatable)",
              args: {name: "tag", isVariadic: true},
              isRepeatable: true,
            },
            {
              name: "--threshold",
              description: "Exit 1 if any case score is below this threshold (default: 1.0)",
              args: {name: "0..1"},
            },
            {
              name: "--trust-plugin",
              description: "Assert that you trust this plugin's code and eval suite, and skip the first-run trust prompt (for CI",
            },
            {
              name: "--verbose",
              description: "Log per-message trace events to the debug log (use --debug-file to read them)",
            },
          ],
          args: {name: "target", isOptional: true},
        },
        {
          name: ["init", "new"],
          description: "Scaffold a new plugin at ~/.claude/skills/<name>/ (auto-loads next session as <name>@skills-dir)",
          options: [
            {
              name: "--author",
              description: "Author name (default: git config user.name)",
              args: {name: "name"},
            },
            {
              name: "--author-email",
              description: "Author email (default: git config user.email)",
              args: {name: "email"},
            },
            {name: "--description", description: "Manifest description", args: {name: "text"}},
            {
              name: ["-f", "--force"],
              description: "Overwrite an existing .claude-plugin/ at the target",
            },
            {
              name: "--with",
              description: "Also scaffold: skills, agents, hooks, mcp, lsp, output-style, channel",
              args: {name: "components", isVariadic: true},
            },
          ],
          args: {name: "name"},
        },
        {
          name: ["install", "i"],
          description: "Install a plugin from available marketplaces (use plugin@marketplace for specific marketplace)",
          options: [
            {
              name: "--accept-command",
              description: "Accept the marketplace-declared command",
              args: {name: "sha256"},
            },
            {
              name: "--config",
              description: "Set a userConfig option declared in the plugin's manifest",
              args: {name: "key=value"},
              isRepeatable: true,
            },
            {
              name: "--json",
              description: "Print one machine-readable result line on stdout instead of the human message (same exit codes",
            },
            {
              name: "--marketplace",
              description: "Install <plugin> (its bare name) from the marketplace at this source",
              args: {name: "source"},
            },
            {name: "--registry", description: "For a <package>@npm install", args: {name: "url"}},
            {
              name: ["-s", "--scope"],
              description: "Installation scope: user, project, or local (default: \"user\")",
              args: {name: "scope", suggestions: ["user", "project", "local"]},
            },
            {
              name: ["-y", "--yes"],
              description: "Accept the displayed marketplace-declared command without the confirmation prompt",
            },
          ],
          args: {name: "plugin"},
        },
        {
          name: "list",
          description: "List installed plugins",
          options: [
            {
              name: "--available",
              description: "Include available plugins from marketplaces (requires --json)",
            },
            {
              name: "--data-size",
              description: "Measure each installed plugin's saved data directory, or only the named plugin's (requires --json)",
              args: {name: "plugin", isOptional: true},
            },
            {name: "--json", description: "Output as JSON"},
          ],
        },
        {
          name: "marketplace",
          description: "Manage Claude Code marketplaces",
          subcommands: [
            {
              name: "add",
              description: "Add a marketplace from a URL, path, or GitHub repo",
              options: [
                {
                  name: "--claudeai",
                  description: "Add the marketplace of this name that claude.ai hosts for you, by its listed name or its local name (see",
                },
                {
                  name: "--json",
                  description: "Print one machine-readable result line as the last line on stdout (same exit codes)",
                },
                {
                  name: "--scope",
                  description: "Where to declare the marketplace: user (default), project, or local",
                  args: {name: "scope", suggestions: ["user", "project", "local"]},
                },
                {
                  name: "--sparse",
                  description: "Limit checkout to specific directories via git sparse-checkout (for monorepos)",
                  args: {name: "paths", isVariadic: true, template: "filepaths"},
                },
              ],
              args: {name: "source"},
            },
            {
              name: "list",
              description: "List all configured marketplaces",
              options: [{name: "--json", description: "Output as JSON"}],
            },
            {
              name: ["remove", "rm"],
              description: "Remove a configured marketplace",
              options: [
                {
                  name: "--json",
                  description: "Print one machine-readable result line as the last line on stdout (same exit codes)",
                },
                {
                  name: "--scope",
                  description: "Remove the marketplace declaration from a specific settings scope: user, project, or local",
                  args: {name: "scope", suggestions: ["user", "project", "local"]},
                },
              ],
              args: {name: "name"},
            },
            {
              name: "update",
              description: "Update marketplace(s) from their source - updates all if no name specified",
              options: [
                {
                  name: "--json",
                  description: "With a marketplace name: print one machine-readable result line as the last line on stdout (same exit codes)",
                },
              ],
              args: {name: "name", isOptional: true},
            },
          ],
        },
        {
          name: ["prune", "autoremove"],
          description: "Remove auto-installed dependencies that are no longer needed",
          options: [
            {name: "--dry-run", description: "List what would be removed without removing"},
            {
              name: ["-s", "--scope"],
              description: "Prune at scope: user, project, or local (default: \"user\")",
              args: {name: "scope", suggestions: ["user", "project", "local"]},
            },
            {
              name: ["-y", "--yes"],
              description: "Skip the confirmation prompt (required when stdin or stdout is not a TTY)",
            },
          ],
        },
        {
          name: "tag",
          description: "Create a {name}--v{version} git tag for a plugin release",
          options: [
            {name: "--dry-run", description: "Print what would be tagged without creating it"},
            {
              name: ["-f", "--force"],
              description: "Skip the dirty-working-tree and tag-already-exists checks",
            },
            {
              name: ["-m", "--message"],
              description: "Tag annotation message (use %s for the version)",
              args: {name: "msg"},
            },
            {name: "--push", description: "Push the tag to --remote after creating it"},
            {
              name: "--remote",
              description: "Remote to push to with --push (default: \"origin\")",
              args: {name: "name"},
            },
          ],
          args: {name: "path", isOptional: true, template: "filepaths"},
        },
        {
          name: "test",
          description: "Run a mod's tests",
          args: {
            name: "dir",
            description: "The mod's folder (default: the current folder)",
            isOptional: true,
            template: "folders",
          },
        },
        {
          name: ["uninstall", "remove"],
          description: "Uninstall an installed plugin",
          options: [
            {
              name: "--json",
              description: "Print one machine-readable result line on stdout instead of the human message (same exit codes",
            },
            {
              name: "--keep-data",
              description: "Preserve the plugin's persistent data directory (~/.claude/plugins/data/{id}/)",
            },
            {
              name: "--prune",
              description: "Also remove auto-installed dependencies that are no longer needed (requires -y in non-interactive contexts)",
            },
            {
              name: ["-s", "--scope"],
              description: "Uninstall from scope: user, project, or local (default: \"user\")",
              args: {name: "scope", suggestions: ["user", "project", "local"]},
            },
            {
              name: ["-y", "--yes"],
              description: "Skip the --prune confirmation prompt (required when stdin or stdout is not a TTY)",
            },
          ],
          args: {name: "plugin"},
        },
        {
          name: "update",
          description: "Update a plugin to the latest version (restart required to apply)",
          options: [
            {
              name: "--accept-command",
              description: "Accept the marketplace-declared command",
              args: {name: "sha256"},
            },
            {
              name: "--json",
              description: "Print one machine-readable result line on stdout instead of the human message (same exit codes",
            },
            {
              name: ["-s", "--scope"],
              description: "Installation scope: user, project, local, managed (default: auto-detect)",
              args: {name: "scope", suggestions: ["user", "project", "local", "managed"]},
            },
            {
              name: ["-y", "--yes"],
              description: "Accept the displayed marketplace-declared command without the confirmation prompt",
            },
          ],
          args: {name: "plugin"},
        },
        {
          name: "validate",
          description: "Validate a plugin or marketplace manifest, or the skills, agents, and commands in a directory",
          options: [
            {name: "--json", description: "Output the validation report as JSON (same exit codes)"},
            {
              name: "--profile",
              description: "Also exit 1 when the plugin directory",
              args: {name: "name", suggestions: ["directory"]},
            },
            {name: "--strict", description: "Treat warnings as errors (exit 1)"},
          ],
          args: {name: "path", template: "filepaths"},
        },
      ],
    },
    {
      name: "purge",
      description: "Delete a project's Claude Code data",
      options: [
        {
          name: "--all",
          description: "Purge state for every project (mutually exclusive with [path])",
        },
        {name: "--dry-run", description: "List what would be deleted without deleting anything"},
        {name: ["-i", "--interactive"], description: "Prompt for each item before deleting"},
        {name: ["-y", "--yes"], description: "Skip confirmation prompt"},
      ],
      args: {name: "path", isOptional: true, template: "filepaths"},
    },
    {
      name: "respawn",
      description: "Restart a background session, or all of them with --all, so it runs the current Claude Code version",
      options: [{name: "--all", description: "Restart all background sessions"}],
      args: {name: "id", isOptional: true},
    },
    {
      name: "rm",
      description: "Delete a background session, and its worktree when that is safe",
      options: [
        {
          name: "--discard-unpushed",
          description: "also discard the worktree's unpushed commits",
          args: {name: "commit@worktree-id"},
        },
        {
          name: "--force-remove-worktree",
          description: "delete the worktree directory even though the WorktreeRemove hook or git couldn't remove it",
          args: {name: "worktree-id"},
        },
      ],
      args: {name: "id"},
    },
    {
      name: "setup-token",
      description: "Set up a long-lived authentication token (requires Claude subscription)",
    },
    {name: ["stop", "kill"], description: "Stop a background session", args: {name: "id"}},
    {
      name: "ultrareview",
      description: "Run a cloud-hosted multi-agent code review of the current branch",
      options: [
        {
          name: "--json",
          description: "Print the raw bugs.json payload instead of formatted findings",
        },
        {name: "--no-post", description: "Do not post the findings to the PR (the default"},
        {
          name: "--post",
          description: "Post the finished review's findings to the PR as you (PR targets only; one plain comment, not a review)",
        },
        {
          name: "--timeout",
          description: "Maximum minutes to wait for the review to finish (default: 45)",
          args: {name: "minutes"},
        },
        {
          name: "--trust-workspace",
          description: "Trust the current folder for this run only, without asking (for scripts and CI)",
        },
      ],
      args: {name: "target", isOptional: true},
    },
    {
      name: ["update", "upgrade"],
      description: "Check for updates and install if available; package-manager installs update through the package manager",
    },
  ],
  options: [
    {
      name: "--add-dir",
      description: "Additional directories to allow tool access to",
      args: {name: "directories", isVariadic: true, template: "folders"},
    },
    {
      name: "--agent",
      description: "Agent for the current session. Overrides the 'agent' setting.",
      args: {name: "agent"},
    },
    {
      name: "--agents",
      description: "JSON object defining custom agents, or with --print the path to a file that holds one (e.g. '{\"reviewer\"",
      args: {name: "json-or-file"},
    },
    {
      name: "--allow-dangerously-skip-permissions",
      description: "Enable bypassing all permission checks as an option, without it being enabled by default",
    },
    {
      name: ["--allowedTools", "--allowed-tools"],
      description: "Comma or space-separated list of tool names to allow (e.g. \"Bash(git *) Edit\")",
      args: {name: "tools", isVariadic: true},
    },
    {
      name: "--append-system-prompt",
      description: "Append a system prompt to the default system prompt",
      args: {name: "prompt"},
    },
    {
      name: "--autocompact",
      description: "Auto-compact window size (auto, or 100k–1M tokens)",
      args: {name: "auto|tokens"},
    },
    {
      name: "--ax-screen-reader",
      description: "Render screen-reader friendly output (flat text, no decorative borders or animations).",
    },
    {
      name: ["--bg", "--background"],
      description: "Start the session in the background and return immediately",
    },
    {
      name: "--bare",
      description: "Minimal mode: skip hooks (those defined in settings and by installed plugins",
    },
    {
      name: "--betas",
      description: "Beta headers to include in API requests (API key users only)",
      args: {name: "betas", isVariadic: true},
    },
    {name: "--brief", description: "Enable SendUserMessage tool for agent-to-user communication"},
    {name: "--chrome", description: "Enable Claude in Chrome integration"},
    {
      name: "--cloud",
      description: "Create a cloud session with the given description",
      args: {name: "description|session_id|url", isOptional: true},
    },
    {
      name: ["-c", "--continue"],
      description: "Continue the most recent conversation in the current directory",
    },
    {
      name: "--dangerously-skip-permissions",
      description: "Bypass all permission checks. Recommended only for sandboxes with no internet access.",
    },
    {
      name: ["-d", "--debug"],
      description: "Enable debug mode with optional category filtering (e.g., --debug=hooks,mcp or --debug='!hooks,!mcp')",
      args: {name: "filter", isOptional: true},
    },
    {
      name: "--debug-file",
      description: "Write debug logs to a specific file path (implicitly enables debug mode)",
      args: {name: "path", template: "filepaths"},
    },
    {
      name: "--desktop",
      description: "Open in the Claude Desktop app instead of the terminal (with --continue or --resume <id> to pick the session)",
    },
    {name: "--disable-slash-commands", description: "Disable all skills and commands"},
    {
      name: ["--disallowedTools", "--disallowed-tools"],
      description: "Comma or space-separated list of tool names to deny (e.g. \"Bash(git *) Edit\")",
      args: {name: "tools", isVariadic: true},
    },
    {
      name: "--effort",
      description: "Effort level for the current session (low, medium, high, xhigh, max)",
      args: {name: "level", suggestions: ["low", "medium", "high", "xhigh", "max"]},
    },
    {
      name: "--environment",
      description: "Create a new cloud session that runs on the given self-hosted environment (ccpool_...).",
      args: {name: "environment_id"},
    },
    {name: "--exclude-dynamic-system-prompt-sections", description: "Move per-machine sections"},
    {
      name: "--fallback-model",
      description: "Enable automatic fallback to specified model(s) when the default model is overloaded or not available",
      args: {name: "model"},
    },
    {
      name: "--file",
      description: "File resources to download at startup",
      args: {name: "specs", isVariadic: true},
    },
    {
      name: "--fork-session",
      description: "When resuming, create a new session ID instead of reusing the original (use with --resume or --continue)",
    },
    {
      name: "--forward-subagent-text",
      description: "Forward subagent text and thinking blocks as assistant/user messages with parent_tool_use_id set",
    },
    {
      name: "--from-pr",
      description: "Resume a session linked to a PR by PR number/URL, or open interactive picker with optional search term",
      args: {name: "value", isOptional: true},
    },
    {name: ["-h", "--help"], description: "Display help for command", isPersistent: true},
    {
      name: "--ide",
      description: "Automatically connect to IDE on startup if exactly one valid IDE is available",
    },
    {
      name: "--include-hook-events",
      description: "Include all hook lifecycle events in the output stream (only works with --output-format=stream-json)",
    },
    {
      name: "--include-partial-messages",
      description: "Include partial message chunks as they arrive (only works with --print and --output-format=stream-json)",
    },
    {
      name: "--input-format",
      description: "Input format (only works with --print): \"text\" (default), or \"stream-json\" (realtime streaming input)",
      args: {name: "format", suggestions: ["text", "stream-json"]},
    },
    {
      name: "--json-schema",
      description: "JSON Schema for structured output validation (only works with --print)",
      args: {name: "schema"},
    },
    {
      name: "--max-budget-usd",
      description: "Maximum dollar amount to spend on API calls (only works with --print)",
      args: {name: "amount"},
    },
    {
      name: "--mcp-config",
      description: "Load MCP servers from JSON files or strings (space-separated)",
      args: {name: "configs", isVariadic: true, template: "filepaths"},
    },
    {
      name: "--model",
      description: "Model for the current session",
      args: {name: "model", suggestions: ["fable", "opus", "sonnet"]},
    },
    {
      name: ["-n", "--name"],
      description: "Set a display name for this session (shown in the prompt box, /resume picker, and terminal title)",
      args: {name: "name"},
    },
    {name: "--no-chrome", description: "Disable Claude in Chrome integration"},
    {
      name: "--no-session-persistence",
      description: "Disable session persistence - sessions will not be saved to disk and cannot be resumed",
    },
    {
      name: "--output-format",
      description: "Output format (only works with --print)",
      args: {name: "format", suggestions: ["text", "json", "stream-json"]},
    },
    {
      name: "--permission-mode",
      description: "Permission mode to use for the session",
      args: {
        name: "mode",
        suggestions: ["acceptEdits", "auto", "bypassPermissions", "manual", "dontAsk", "plan"],
      },
    },
    {
      name: "--permission-prompts",
      description: "Who answers permission prompts with --print",
      args: {name: "target", suggestions: ["host", "none"]},
    },
    {
      name: "--plugin-dir",
      description: "Load a plugin from a directory or .zip for this session only",
      args: {name: "path", template: "filepaths"},
      isRepeatable: true,
    },
    {
      name: "--plugin-url",
      description: "Fetch a plugin .zip from a URL for this session only (repeatable: --plugin-url A --plugin-url B) (default: [])",
      args: {name: "url"},
      isRepeatable: true,
    },
    {name: ["-p", "--print"], description: "Print response and exit (useful for pipes)"},
    {
      name: "--prompt-suggestions",
      description: "Enable prompt suggestions",
      args: {
        name: "value",
        isOptional: true,
        suggestions: ["true", "false", "1", "0", "yes", "no", "on", "off"],
      },
    },
    {
      name: "--remote-control",
      description: "Start an interactive session with Remote Control enabled (optionally named)",
      args: {name: "name", isOptional: true},
    },
    {
      name: "--remote-control-session-name-prefix",
      description: "Prefix for auto-generated Remote Control session names (default: hostname)",
      args: {name: "prefix"},
    },
    {
      name: "--replay-user-messages",
      description: "Re-emit user messages from stdin back on stdout for acknowledgment",
    },
    {
      name: "--restricted",
      description: "Restricted mode: removes the built-in tools that run commands or code",
    },
    {
      name: ["-r", "--resume"],
      description: "Resume a conversation by session ID or name, or open interactive picker with optional search term",
      args: {name: "value", isOptional: true},
    },
    {name: "--safe-mode", description: "Start with all customizations"},
    {
      name: "--session-id",
      description: "Use a specific session ID for the conversation (must be a valid UUID)",
      args: {name: "uuid"},
    },
    {
      name: "--setting-sources",
      description: "Comma-separated list of setting sources to load (user, project, local).",
      args: {name: "sources"},
    },
    {
      name: "--settings",
      description: "Path to a settings JSON file or a JSON string to load additional settings from",
      args: {name: "file-or-json", template: "filepaths"},
    },
    {
      name: "--strict-mcp-config",
      description: "Only use MCP servers from --mcp-config, ignoring all other MCP configurations",
    },
    {
      name: "--system-prompt",
      description: "System prompt to use for the session",
      args: {name: "prompt"},
    },
    {
      name: "--system-prompt-snapshot",
      description: "Record the system prompt once per conversation and reuse it verbatim on every request and resume. on",
      args: {name: "system-prompt-snapshot", suggestions: ["on", "off"]},
    },
    {
      name: "--teleport",
      description: "Resume a teleport session, optionally specify session ID",
      args: {name: "session", isOptional: true},
    },
    {name: "--tmux", description: "Create a tmux session for the worktree (requires --worktree)"},
    {
      name: "--tools",
      description: "Specify the list of available tools from the built-in set",
      args: {name: "tools", isVariadic: true},
    },
    {name: "--verbose", description: "Override verbose mode setting from config"},
    {name: ["-v", "--version"], description: "Output the version number"},
    {
      name: ["-w", "--worktree"],
      description: "Create a new git worktree for this session (optionally specify a name)",
      args: {name: "name", isOptional: true},
    },
  ],
  args: {name: "prompt", description: "Your prompt", isOptional: true},
};
