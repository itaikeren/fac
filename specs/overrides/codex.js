// From codex --help and the help of each subcommand, Codex CLI 0.166.0, 2026-11-02.
export default {
  name: "codex",
  subcommands: [
    {
      name: "agents",
      description: "Browse all agent sessions on the shared local app-server daemon",
      options: [
        {
          name: "--remote",
          description: "Connect the TUI to a remote app server endpoint.",
          args: {name: "addr"},
        },
        {
          name: "--remote-auth-token-env",
          description: "Name of the environment variable containing the bearer token to send to a remote app server websocket",
          args: {name: "env_var"},
        },
        {
          name: ["-C", "--cd"],
          description: "Use this directory for new tasks on a remote server",
          args: {name: "dir", template: "folders"},
        },
        {name: "--no-alt-screen", description: "Disable alternate screen mode"},
      ],
    },
    {
      name: ["exec", "e"],
      description: "Run Codex non-interactively",
      subcommands: [
        {
          name: "resume",
          description: "Resume a previous session by id or pick the most recent with --last",
          options: [
            {
              name: "--last",
              description: "Resume the most recent recorded session (newest) without specifying an id",
            },
            {name: "--all", description: "Show all sessions (disables cwd filtering)"},
            {
              name: ["-i", "--image"],
              description: "Optional image(s) to attach to the prompt sent after resuming",
              args: {name: "file", template: "filepaths"},
            },
          ],
          args: [
            {
              name: "session_id",
              description: "Conversation/session id (UUID) or thread name",
              isOptional: true,
            },
            {
              name: "prompt",
              description: "Prompt to send after resuming the session. If `-` is used, read from stdin",
              isOptional: true,
            },
          ],
        },
        {
          name: "fork",
          description: "Fork a previous session by id into a new session",
          options: [
            {
              name: ["-i", "--image"],
              description: "Optional image(s) to attach to the prompt sent after forking",
              args: {name: "file", template: "filepaths"},
            },
          ],
          args: [
            {
              name: "session_id",
              description: "Conversation/session id (UUID) or thread name to fork",
            },
            {
              name: "prompt",
              description: "Optional prompt to send after forking. If `-` is used, read from stdin",
              isOptional: true,
            },
          ],
        },
        {
          name: "review",
          description: "Run a code review against the current repository",
          options: [
            {name: "--uncommitted", description: "Review staged, unstaged, and untracked changes"},
            {
              name: "--base",
              description: "Review changes against the given base branch",
              args: {name: "branch"},
            },
            {
              name: "--commit",
              description: "Review the changes introduced by a commit",
              args: {name: "sha"},
            },
            {
              name: "--title",
              description: "Optional commit title to display in the review summary",
              args: {name: "title"},
            },
          ],
          args: {
            name: "prompt",
            description: "Custom review instructions. If `-` is used, read from stdin",
            isOptional: true,
          },
        },
      ],
      options: [
        {
          name: "--strict-config",
          description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
          isPersistent: true,
        },
        {
          name: ["-i", "--image"],
          description: "Optional image(s) to attach to the initial prompt",
          args: {name: "file", isVariadic: true, template: "filepaths"},
        },
        {
          name: ["-m", "--model"],
          description: "Model the agent should use",
          args: {name: "model"},
          isPersistent: true,
        },
        {name: "--oss", description: "Use open-source provider"},
        {
          name: "--local-provider",
          description: "Specify which local provider to use (lmstudio or ollama)",
          args: {name: "oss_provider", suggestions: ["lmstudio", "ollama"]},
        },
        {
          name: ["-p", "--profile"],
          description: "Layer $CODEX_HOME/<name>.config.toml on top of the base user config",
          args: {name: "config_profile_v2"},
        },
        {
          name: ["-s", "--sandbox"],
          description: "Select the sandbox policy to use when executing model-generated shell commands",
          args: {
            name: "sandbox_mode",
            suggestions: ["read-only", "workspace-write", "danger-full-access"],
          },
        },
        {
          name: "--approve-for-me",
          description: "Route approval requests through automatic review using the workspace-write sandbox",
        },
        {
          name: "--dangerously-bypass-approvals-and-sandbox",
          description: "Skip all confirmation prompts and execute commands without sandboxing",
          isPersistent: true,
        },
        {
          name: "--dangerously-bypass-hook-trust",
          description: "Run enabled hooks without requiring persisted hook trust for this invocation",
          isPersistent: true,
        },
        {
          name: ["-C", "--cd"],
          description: "Tell the agent to use the specified directory as its working root",
          args: {name: "dir", template: "folders"},
        },
        {
          name: "--worktree",
          description: "Run the session in a new managed Git worktree",
          isPersistent: true,
        },
        {
          name: "--add-dir",
          description: "Additional directories that should be writable alongside the primary workspace",
          args: {name: "dir", template: "folders"},
        },
        {
          name: "--thread-source",
          description: "Source classification for newly created or forked threads",
          args: {name: "source"},
          isPersistent: true,
        },
        {
          name: "--cyber-access-program",
          description: "Request an experimental Cyber access program for this turn (OpenAI provider only)",
          args: {name: "program", suggestions: ["standard", "daybreak_blue", "daybreak_red"]},
          isPersistent: true,
        },
        {
          name: "--skip-git-repo-check",
          description: "Allow running Codex outside a Git repository",
          isPersistent: true,
        },
        {
          name: "--ephemeral",
          description: "Run without persisting session files to disk",
          isPersistent: true,
        },
        {
          name: "--ignore-user-config",
          description: "Do not load `$CODEX_HOME/config.toml`; auth still uses `CODEX_HOME`",
          isPersistent: true,
        },
        {
          name: "--ignore-rules",
          description: "Do not load user or project execpolicy `.rules` files",
          isPersistent: true,
        },
        {
          name: "--output-schema",
          description: "Path to a JSON Schema file describing the model's final response shape",
          args: {name: "file", template: "filepaths"},
          isPersistent: true,
        },
        {
          name: "--color",
          description: "Specifies color settings for use in the output",
          args: {name: "color", suggestions: ["always", "never", "auto"]},
        },
        {name: "--json", description: "Print events to stdout as JSONL", isPersistent: true},
        {
          name: ["-o", "--output-last-message"],
          description: "Specifies file where the last message from the agent should be written",
          args: {name: "file", template: "filepaths"},
          isPersistent: true,
        },
        {name: ["-V", "--version"], description: "Print version"},
      ],
      args: {name: "prompt", description: "Initial instructions for the agent", isOptional: true},
    },
    {
      name: "review",
      description: "Run a code review non-interactively",
      options: [
        {
          name: "--strict-config",
          description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
        },
        {name: "--uncommitted", description: "Review staged, unstaged, and untracked changes"},
        {
          name: "--base",
          description: "Review changes against the given base branch",
          args: {name: "branch"},
        },
        {
          name: "--commit",
          description: "Review the changes introduced by a commit",
          args: {name: "sha"},
        },
        {
          name: "--title",
          description: "Optional commit title to display in the review summary",
          args: {name: "title"},
        },
      ],
      args: {
        name: "prompt",
        description: "Custom review instructions. If `-` is used, read from stdin",
        isOptional: true,
      },
    },
    {
      name: "login",
      description: "Manage login",
      subcommands: [{name: "status", description: "Show login status"}],
      options: [
        {
          name: "--with-api-key",
          description: "Read the API key from stdin (e.g. `printenv OPENAI_API_KEY | codex login --with-api-key`)",
        },
        {
          name: "--with-access-token",
          description: "Read the access token from stdin (e.g. `printenv CODEX_ACCESS_TOKEN | codex login --with-access-token`)",
        },
        {name: "--device-auth"},
      ],
    },
    {name: "logout", description: "Remove stored authentication credentials"},
    {
      name: "mcp",
      description: "Manage external MCP servers for Codex",
      subcommands: [
        {
          name: "list",
          options: [{name: "--json", description: "Output the configured servers as JSON"}],
        },
        {
          name: "get",
          options: [{name: "--json", description: "Output the server configuration as JSON"}],
          args: {name: "name", description: "Name of the MCP server to display"},
        },
        {
          name: "add",
          options: [
            {
              name: "--env",
              description: "Environment variables to set when launching the server. Only valid with stdio servers",
              args: {name: "KEY=VALUE"},
            },
            {
              name: "--url",
              description: "URL for a streamable HTTP MCP server",
              args: {name: "url"},
            },
            {
              name: "--bearer-token-env-var",
              description: "Optional environment variable to read for a bearer token. Only valid with streamable HTTP servers",
              args: {name: "env_var"},
            },
            {
              name: "--oauth-client-id",
              description: "Optional OAuth client identifier to use for this MCP server",
              args: {name: "client_id"},
            },
            {
              name: "--oauth-client-secret",
              description: "Optional OAuth client secret for the pre-registered client",
              args: {name: "client_secret"},
            },
            {
              name: "--oauth-client-registration",
              description: "OAuth client-registration strategy for the immediate login only",
              args: {name: "oauth-client-registration", suggestions: ["auto", "cimd", "dcr"]},
            },
            {
              name: "--oauth-resource",
              description: "Optional OAuth resource parameter to include during MCP login",
              args: {name: "resource"},
            },
          ],
          args: [
            {name: "name", description: "Name for the MCP server configuration"},
            {
              name: "command",
              description: "Command to launch the MCP server. Use --url for a streamable HTTP server",
              isOptional: true,
              isVariadic: true,
            },
          ],
        },
        {
          name: "remove",
          args: {name: "name", description: "Name of the MCP server configuration to remove"},
        },
        {
          name: "login",
          options: [
            {
              name: "--no-browser",
              description: "Print the authorization URL and accept the callback URL without opening a browser",
            },
            {
              name: "--scopes",
              description: "Comma-separated list of OAuth scopes to request",
              args: {name: "SCOPE,SCOPE"},
            },
            {
              name: "--oauth-client-registration",
              description: "OAuth client-registration strategy for this login only",
              args: {name: "oauth-client-registration", suggestions: ["auto", "cimd", "dcr"]},
            },
          ],
          args: {name: "name", description: "Name of the MCP server to authenticate with oauth"},
        },
        {
          name: "logout",
          args: {name: "name", description: "Name of the MCP server to deauthenticate"},
        },
      ],
    },
    {
      name: "plugin",
      description: "Manage Codex plugins",
      subcommands: [
        {
          name: "add",
          description: "Install a plugin from a configured or remote marketplace",
          options: [
            {
              name: ["-m", "--marketplace"],
              description: "Marketplace name to use when PLUGIN does not include @MARKETPLACE",
              args: {name: "marketplace"},
            },
            {name: "--json", description: "Output install result as JSON"},
          ],
          args: {
            name: "plugin[@marketplace]",
            description: "Plugin selector to install: either PLUGIN@MARKETPLACE or PLUGIN with --marketplace",
          },
        },
        {
          name: "list",
          description: "List plugins available from configured and remote marketplaces",
          options: [
            {
              name: ["-m", "--marketplace"],
              description: "Only list plugins from this marketplace name",
              args: {name: "marketplace"},
            },
            {name: "--json", description: "Output plugin list as JSON"},
            {
              name: "--available",
              description: "Include uninstalled marketplace plugins in the JSON output",
            },
          ],
        },
        {
          name: "marketplace",
          description: "Add, list, upgrade, or remove configured plugin marketplaces",
          subcommands: [
            {
              name: "add",
              description: "Add a local or Git marketplace to the configured marketplace sources",
              options: [
                {
                  name: "--ref",
                  description: "Git ref to fetch for Git marketplace sources",
                  args: {name: "ref"},
                },
                {
                  name: "--sparse",
                  description: "Sparse checkout path for Git marketplace sources. Can be repeated",
                  args: {name: "path", template: "filepaths"},
                  isRepeatable: true,
                },
                {name: "--json", description: "Output add result as JSON"},
              ],
              args: {
                name: "source",
                description: "Marketplace source: a local path, owner/repo[@ref], HTTPS Git URL, or SSH Git URL",
              },
            },
            {
              name: "list",
              description: "List plugin marketplaces Codex is currently considering and their roots",
              options: [{name: "--json", description: "Output marketplace list as JSON"}],
            },
            {
              name: "upgrade",
              description: "Refresh configured Git marketplace snapshots",
              options: [{name: "--json", description: "Output upgrade result as JSON"}],
              args: {
                name: "marketplace_name",
                description: "Optional configured marketplace name to upgrade. Omit to upgrade all Git marketplaces",
                isOptional: true,
              },
            },
            {
              name: "remove",
              description: "Remove a configured marketplace source by name",
              options: [{name: "--json", description: "Output remove result as JSON"}],
              args: {name: "marketplace_name", description: "Configured marketplace name to remove"},
            },
          ],
        },
        {
          name: "remove",
          description: "Uninstall a plugin and remove its local cache",
          options: [
            {
              name: ["-m", "--marketplace"],
              description: "Marketplace name to use when PLUGIN does not include @MARKETPLACE",
              args: {name: "marketplace"},
            },
            {name: "--json", description: "Output remove result as JSON"},
          ],
          args: {
            name: "plugin[@marketplace]",
            description: "Plugin selector to remove: either PLUGIN@MARKETPLACE or PLUGIN with --marketplace",
          },
        },
      ],
    },
    {
      name: "app-server",
      description: "[experimental] Run the app server or related tooling",
      subcommands: [
        {
          name: "daemon",
          description: "Manage the local app-server daemon",
          subcommands: [
            {
              name: "bootstrap",
              description: "Install durable local app-server management for SSH-driven use",
              options: [
                {
                  name: "--remote-control",
                  description: "Launch the managed app-server with remote control enabled",
                },
              ],
            },
            {
              name: "start",
              description: "Start the local app server daemon if it is not already running",
            },
            {name: "restart", description: "Restart the local app server daemon"},
            {
              name: "update",
              description: "Update the daemon package (may interrupt running work)",
              options: [
                {name: "--from-cli", description: "Copy and pin this CLI package"},
                {
                  name: ["-y", "--yes"],
                  description: "Confirm replacing the daemon package without an interactive prompt",
                },
              ],
            },
            {
              name: "enable-remote-control",
              description: "Enable remote control for future starts and a currently running managed daemon",
            },
            {
              name: "disable-remote-control",
              description: "Disable remote control for future starts and a currently running managed daemon",
            },
            {name: "stop", description: "Stop the local app server daemon"},
            {
              name: "version",
              description: "Print local CLI and running app-server versions as JSON",
            },
          ],
        },
        {
          name: "proxy",
          description: "Proxy stdio bytes to the running app-server control socket",
          options: [
            {
              name: "--sock",
              description: "Path to the app-server Unix domain socket to connect to",
              args: {name: "socket_path", template: "filepaths"},
            },
          ],
        },
        {
          name: "generate-ts",
          description: "[experimental] Generate TypeScript bindings for the app server protocol",
          options: [
            {
              name: ["-o", "--out"],
              description: "Output directory where .ts files will be written",
              args: {name: "dir", template: "folders"},
            },
            {
              name: ["-p", "--prettier"],
              description: "Optional path to the Prettier executable to format generated files",
              args: {name: "prettier_bin"},
            },
            {
              name: "--experimental",
              description: "Include experimental methods and fields in the generated output",
            },
          ],
        },
        {
          name: "generate-json-schema",
          description: "[experimental] Generate JSON Schema for the app server protocol",
          options: [
            {
              name: ["-o", "--out"],
              description: "Output directory where the schema bundle will be written",
              args: {name: "dir", template: "folders"},
            },
            {
              name: "--experimental",
              description: "Include experimental methods and fields in the generated output",
            },
          ],
        },
      ],
      options: [
        {
          name: "--startup-sign-out",
          description: "Sign out using the managed auth path before loading fresh startup requirements",
        },
        {
          name: "--print-startup-config",
          description: "Validate configuration and print requirements as JSON without starting the app server",
        },
        {
          name: "--startup-client-info",
          description: "ClientInfo JSON used for configuration/auth request identity",
          args: {name: "startup_client_info"},
        },
        {
          name: "--cloud-skills-snapshot",
          description: "Load frozen cloud skills from a JSON snapshot instead of a live skill service",
          args: {name: "path", template: "filepaths"},
        },
        {
          name: "--code-mode-host",
          description: "Connect to a remote code-mode host instead of starting a local host",
          args: {name: "url"},
        },
        {
          name: "--strict-config",
          description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
        },
        {name: "--listen", description: "Transport endpoint URL", args: {name: "url"}},
        {
          name: "--stdio",
          description: "Use stdio as the transport (equivalent to `--listen stdio://`)",
        },
        {
          name: "--analytics-default-enabled",
          description: "Controls whether analytics are enabled by default.",
        },
        {
          name: "--ws-auth",
          description: "Authentication mode for incoming WebSocket connections",
          args: {name: "mode", suggestions: ["capability-token", "signed-bearer-token"]},
        },
        {
          name: "--ws-token-file",
          description: "Absolute path to the capability-token file",
          args: {name: "path", template: "filepaths"},
        },
        {
          name: "--ws-token-sha256",
          description: "Hex-encoded SHA-256 digest of the capability token",
          args: {name: "hex"},
        },
        {
          name: "--ws-shared-secret-file",
          description: "Absolute path to the shared secret file for signed JWT bearer tokens",
          args: {name: "path", template: "filepaths"},
        },
        {
          name: "--ws-issuer",
          description: "Expected issuer for signed JWT bearer tokens",
          args: {name: "issuer"},
        },
        {
          name: "--ws-audience",
          description: "Expected audience for signed JWT bearer tokens",
          args: {name: "audience"},
        },
        {
          name: "--ws-max-clock-skew-seconds",
          description: "Maximum clock skew when validating signed JWT bearer tokens",
          args: {name: "seconds"},
        },
      ],
    },
    {
      name: "remote-control",
      description: "[experimental] Manage the app-server daemon with remote control enabled",
      subcommands: [
        {name: "start", description: "Start the app-server daemon with remote control enabled"},
        {name: "stop", description: "Stop the app-server daemon"},
        {name: "pair", description: "Create and print a short-lived manual pairing code"},
      ],
      options: [
        {name: "--json", description: "Emit machine-readable JSON", isPersistent: true},
        {
          name: "--no-daemon",
          description: "Run an embedded app-server in the foreground until Ctrl-C",
        },
      ],
    },
    {
      name: "app",
      description: "Launch the Desktop app (opens the app installer if missing)",
      options: [
        {
          name: "--download-url",
          description: "Override the app installer download URL (advanced)",
          args: {name: "download_url_override"},
        },
      ],
      args: {
        name: "path",
        description: "Workspace path to open in the Desktop app",
        isOptional: true,
        template: "filepaths",
      },
    },
    {
      name: "completion",
      description: "Generate shell completion scripts",
      args: {
        name: "shell",
        description: "Shell to generate completions for",
        isOptional: true,
        suggestions: ["bash", "elvish", "fish", "powershell", "zsh"],
      },
    },
    {name: "update", description: "Update Codex to the latest version"},
    {
      name: "doctor",
      description: "Diagnose local Codex installation, config, auth, and runtime health",
      options: [
        {name: "--json", description: "Emit a redacted machine-readable report"},
        {
          name: "--network-only",
          description: "Run config, network environment, provider HTTP, and Responses WebSocket checks",
        },
        {name: "--summary", description: "Only show grouped check rows and the final count summary"},
        {name: "--all", description: "Expand long lists in detailed human output"},
        {name: "--no-color", description: "Disable ANSI color in human output"},
        {name: "--ascii", description: "Use ASCII status labels and separators in human output"},
      ],
    },
    {
      name: "sandbox",
      description: "Run commands within a Codex-provided sandbox",
      subcommands: [
        {
          name: "uninstall",
          description: "Remove the legacy Windows sandbox's machine-wide accounts and network rules",
        },
      ],
      options: [
        {
          name: "--sandbox-state-json",
          description: "Opaque JSON value from `codex/sandbox-state` to apply directly",
          args: {name: "json"},
        },
        {
          name: "--sandbox-state-readable-root",
          description: "Add a readable root to the supplied sandbox state. Repeat for multiple roots",
          args: {name: "sandbox_state_readable_root"},
        },
        {
          name: "--sandbox-state-disable-network",
          description: "Disable direct network access in the supplied sandbox state",
        },
        {
          name: ["-P", "--permission-profile"],
          description: "Named permissions profile to apply from the active configuration stack",
          args: {name: "name"},
        },
        {
          name: ["-p", "--profile"],
          description: "Layer $CODEX_HOME/<name>.config.toml on top of the base user config",
          args: {name: "config_profile"},
        },
        {
          name: ["-C", "--cd"],
          description: "Working directory used for profile resolution and command execution",
          args: {name: "dir", template: "folders"},
        },
        {
          name: "--include-managed-config",
          description: "Include managed requirements while resolving an explicit permissions profile",
        },
        {
          name: "--allow-unix-socket",
          description: "Allow the sandboxed command to bind/connect AF_UNIX sockets rooted at this path",
          args: {name: "allow_unix_sockets"},
        },
        {
          name: "--log-denials",
          description: "While the command runs, capture macOS sandbox denials via `log stream` and print them after exit",
        },
      ],
    },
    {
      name: "debug",
      description: "Debugging tools",
      subcommands: [
        {
          name: "models",
          description: "Render the raw model catalog as JSON",
          options: [
            {
              name: "--bundled",
              description: "Skip refresh and dump only the bundled catalog shipped with this binary",
            },
          ],
        },
        {
          name: "app-server",
          description: "Tooling: helps debug the app server",
          subcommands: [{name: "send-message-v2", args: {name: "user_message"}}],
        },
        {
          name: "prompt-input",
          description: "Render the model-visible prompt input list as JSON",
          options: [
            {
              name: ["-i", "--image"],
              description: "Optional image(s) to attach to the user prompt",
              args: {name: "file", isVariadic: true, template: "filepaths"},
            },
          ],
          args: {
            name: "prompt",
            description: "Optional user prompt to append after session context",
            isOptional: true,
          },
        },
      ],
    },
    {
      name: ["apply", "a"],
      description: "Apply the latest diff produced by Codex agent as a `git apply` to your local working tree",
      args: {name: "task_id"},
    },
    {
      name: "resume",
      description: "Resume a previous interactive session (picker by default; use --last to continue the most recent)",
      options: [
        {name: "--last", description: "Continue the most recent session without showing the picker"},
        {
          name: "--all",
          description: "Show all sessions (disables cwd filtering and shows CWD column)",
        },
        {
          name: "--include-non-interactive",
          description: "Include non-interactive sessions in the resume picker and --last selection",
        },
        {
          name: "--remote",
          description: "Connect the TUI to a remote app server endpoint.",
          args: {name: "addr"},
        },
        {
          name: "--remote-auth-token-env",
          description: "Name of the environment variable containing the bearer token to send to a remote app server websocket",
          args: {name: "env_var"},
        },
        {
          name: "--strict-config",
          description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
        },
        {
          name: ["-i", "--image"],
          description: "Optional image(s) to attach to the initial prompt",
          args: {name: "file", isVariadic: true, template: "filepaths"},
        },
        {name: ["-m", "--model"], description: "Model the agent should use", args: {name: "model"}},
        {name: "--oss", description: "Use open-source provider"},
        {
          name: "--local-provider",
          description: "Specify which local provider to use (lmstudio or ollama)",
          args: {name: "oss_provider", suggestions: ["lmstudio", "ollama"]},
        },
        {
          name: ["-p", "--profile"],
          description: "Layer $CODEX_HOME/<name>.config.toml on top of the base user config",
          args: {name: "config_profile_v2"},
        },
        {
          name: ["-s", "--sandbox"],
          description: "Select the sandbox policy to use when executing model-generated shell commands",
          args: {
            name: "sandbox_mode",
            suggestions: ["read-only", "workspace-write", "danger-full-access"],
          },
        },
        {
          name: "--approve-for-me",
          description: "Route approval requests through automatic review using the workspace-write sandbox",
        },
        {
          name: "--dangerously-bypass-approvals-and-sandbox",
          description: "Skip all confirmation prompts and execute commands without sandboxing",
        },
        {
          name: "--dangerously-bypass-hook-trust",
          description: "Run enabled hooks without requiring persisted hook trust for this invocation",
        },
        {
          name: ["-C", "--cd"],
          description: "Tell the agent to use the specified directory as its working root",
          args: {name: "dir", template: "folders"},
        },
        {name: "--worktree", description: "Run the session in a new managed Git worktree"},
        {
          name: "--add-dir",
          description: "Additional directories that should be writable alongside the primary workspace",
          args: {name: "dir", template: "folders"},
        },
        {
          name: ["-a", "--ask-for-approval"],
          description: "Configure when the model requires human approval before executing a command",
          args: {
            name: "approval_policy",
            suggestions: [
              {
                name: "on-request",
                description: "The model decides when to ask the user for approval",
              },
              {
                name: "never",
                description: "Never ask for user approval Execution failures are immediately returned to the model",
              },
            ],
          },
        },
        {name: "--search", description: "Enable live web search"},
        {name: "--no-alt-screen", description: "Disable alternate screen mode"},
        {
          name: "--no-daemon",
          description: "Run without the shared background server, even if it is already running",
        },
        {name: ["-V", "--version"], description: "Print version"},
      ],
      args: [
        {name: "session_id", description: "Session id (UUID) or session name", isOptional: true},
        {name: "prompt", description: "Optional user prompt to start the session", isOptional: true},
      ],
    },
    {
      name: "queue",
      description: "Queue a message for an existing session",
      options: [
        {
          name: "--thread",
          description: "Session UUID or exact session name",
          args: {name: "thread"},
        },
        {name: "--message", description: "Message text to queue", args: {name: "text"}},
        {
          name: "--resume",
          description: "After queueing, try to resume the thread on the local daemon or selected --remote server",
        },
        {
          name: "--remote",
          description: "Connect the TUI to a remote app server endpoint.",
          args: {name: "addr"},
        },
        {
          name: "--remote-auth-token-env",
          description: "Name of the environment variable containing the bearer token to send to a remote app server websocket",
          args: {name: "env_var"},
        },
        {
          name: ["-p", "--profile"],
          description: "Layer $CODEX_HOME/<name>.config.toml on top of the base user config",
          args: {name: "config_profile_v2"},
        },
        {
          name: "--strict-config",
          description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
        },
        {
          name: ["-c", "--config"],
          description: "Override a startup configuration value (key=value). Queued messages use the thread's model.",
          args: {name: "key=value"},
        },
        {
          name: ["-C", "--cd"],
          description: "Tell the agent to use the specified directory as its working root",
          args: {name: "dir", template: "folders"},
        },
      ],
    },
    {
      name: "archive",
      description: "Archive a saved session by id or session name",
      options: [
        {
          name: "--remote",
          description: "Connect the TUI to a remote app server endpoint.",
          args: {name: "addr"},
        },
        {
          name: "--remote-auth-token-env",
          description: "Name of the environment variable containing the bearer token to send to a remote app server websocket",
          args: {name: "env_var"},
        },
        {
          name: ["-i", "--image"],
          description: "Optional image(s) to attach to the initial prompt",
          args: {name: "file", isVariadic: true, template: "filepaths"},
        },
        {name: ["-m", "--model"], description: "Model the agent should use", args: {name: "model"}},
        {name: "--oss", description: "Use open-source provider"},
        {
          name: "--local-provider",
          description: "Specify which local provider to use (lmstudio or ollama)",
          args: {name: "oss_provider", suggestions: ["lmstudio", "ollama"]},
        },
        {
          name: ["-p", "--profile"],
          description: "Layer $CODEX_HOME/<name>.config.toml on top of the base user config",
          args: {name: "config_profile_v2"},
        },
        {
          name: ["-s", "--sandbox"],
          description: "Select the sandbox policy to use when executing model-generated shell commands",
          args: {
            name: "sandbox_mode",
            suggestions: ["read-only", "workspace-write", "danger-full-access"],
          },
        },
        {
          name: "--approve-for-me",
          description: "Route approval requests through automatic review using the workspace-write sandbox",
        },
        {
          name: "--dangerously-bypass-approvals-and-sandbox",
          description: "Skip all confirmation prompts and execute commands without sandboxing",
        },
        {
          name: "--dangerously-bypass-hook-trust",
          description: "Run enabled hooks without requiring persisted hook trust for this invocation",
        },
        {
          name: ["-C", "--cd"],
          description: "Tell the agent to use the specified directory as its working root",
          args: {name: "dir", template: "folders"},
        },
        {name: "--worktree", description: "Run the session in a new managed Git worktree"},
        {
          name: "--add-dir",
          description: "Additional directories that should be writable alongside the primary workspace",
          args: {name: "dir", template: "folders"},
        },
        {
          name: "--strict-config",
          description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
        },
      ],
      args: {
        name: "session",
        description: "Session id (UUID) or session name. UUIDs take precedence if it parses",
      },
    },
    {
      name: "delete",
      description: "Permanently delete a saved session by id or session name",
      options: [
        {
          name: "--remote",
          description: "Connect the TUI to a remote app server endpoint.",
          args: {name: "addr"},
        },
        {
          name: "--remote-auth-token-env",
          description: "Name of the environment variable containing the bearer token to send to a remote app server websocket",
          args: {name: "env_var"},
        },
        {
          name: ["-i", "--image"],
          description: "Optional image(s) to attach to the initial prompt",
          args: {name: "file", isVariadic: true, template: "filepaths"},
        },
        {name: ["-m", "--model"], description: "Model the agent should use", args: {name: "model"}},
        {name: "--oss", description: "Use open-source provider"},
        {
          name: "--local-provider",
          description: "Specify which local provider to use (lmstudio or ollama)",
          args: {name: "oss_provider", suggestions: ["lmstudio", "ollama"]},
        },
        {
          name: ["-p", "--profile"],
          description: "Layer $CODEX_HOME/<name>.config.toml on top of the base user config",
          args: {name: "config_profile_v2"},
        },
        {
          name: ["-s", "--sandbox"],
          description: "Select the sandbox policy to use when executing model-generated shell commands",
          args: {
            name: "sandbox_mode",
            suggestions: ["read-only", "workspace-write", "danger-full-access"],
          },
        },
        {
          name: "--approve-for-me",
          description: "Route approval requests through automatic review using the workspace-write sandbox",
        },
        {
          name: "--dangerously-bypass-approvals-and-sandbox",
          description: "Skip all confirmation prompts and execute commands without sandboxing",
        },
        {
          name: "--dangerously-bypass-hook-trust",
          description: "Run enabled hooks without requiring persisted hook trust for this invocation",
        },
        {
          name: ["-C", "--cd"],
          description: "Tell the agent to use the specified directory as its working root",
          args: {name: "dir", template: "folders"},
        },
        {name: "--worktree", description: "Run the session in a new managed Git worktree"},
        {
          name: "--add-dir",
          description: "Additional directories that should be writable alongside the primary workspace",
          args: {name: "dir", template: "folders"},
        },
        {
          name: "--strict-config",
          description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
        },
        {name: "--force", description: "Delete without prompting. SESSION must be a UUID"},
      ],
      args: {
        name: "session",
        description: "Session id (UUID) or session name. UUIDs take precedence if it parses",
      },
    },
    {
      name: "migrate-rollouts",
      description: "Inspect or migrate legacy local sessions to paginated thread history",
      options: [
        {
          name: "--apply",
          description: "Publish the migration. Without this flag the command only reports eligible sessions",
        },
        {
          name: "--thread",
          description: "Restrict inspection or migration to one or more thread IDs",
          args: {name: "thread_id"},
        },
        {
          name: "--max-mib-per-second",
          description: "Limit aggregate rollout read and write throughput, in MiB per second",
          args: {name: "mib"},
        },
        {name: "--json", description: "Emit the complete per-thread report as JSON"},
        {name: "--verbose", description: "Print one line for every inspected rollout"},
      ],
    },
    {
      name: "migrate-nfs-db",
      description: "[experimental] Migrate configured SQLite databases to DELETE mode in place",
      options: [
        {
          name: "--staging-dir",
          description: "Use a local directory with space for the database and WAL files",
          args: {name: "staging_dir", template: "folders"},
        },
      ],
    },
    {
      name: "unarchive",
      description: "Unarchive a saved session by id or session name",
      options: [
        {
          name: "--remote",
          description: "Connect the TUI to a remote app server endpoint.",
          args: {name: "addr"},
        },
        {
          name: "--remote-auth-token-env",
          description: "Name of the environment variable containing the bearer token to send to a remote app server websocket",
          args: {name: "env_var"},
        },
        {
          name: ["-i", "--image"],
          description: "Optional image(s) to attach to the initial prompt",
          args: {name: "file", isVariadic: true, template: "filepaths"},
        },
        {name: ["-m", "--model"], description: "Model the agent should use", args: {name: "model"}},
        {name: "--oss", description: "Use open-source provider"},
        {
          name: "--local-provider",
          description: "Specify which local provider to use (lmstudio or ollama)",
          args: {name: "oss_provider", suggestions: ["lmstudio", "ollama"]},
        },
        {
          name: ["-p", "--profile"],
          description: "Layer $CODEX_HOME/<name>.config.toml on top of the base user config",
          args: {name: "config_profile_v2"},
        },
        {
          name: ["-s", "--sandbox"],
          description: "Select the sandbox policy to use when executing model-generated shell commands",
          args: {
            name: "sandbox_mode",
            suggestions: ["read-only", "workspace-write", "danger-full-access"],
          },
        },
        {
          name: "--approve-for-me",
          description: "Route approval requests through automatic review using the workspace-write sandbox",
        },
        {
          name: "--dangerously-bypass-approvals-and-sandbox",
          description: "Skip all confirmation prompts and execute commands without sandboxing",
        },
        {
          name: "--dangerously-bypass-hook-trust",
          description: "Run enabled hooks without requiring persisted hook trust for this invocation",
        },
        {
          name: ["-C", "--cd"],
          description: "Tell the agent to use the specified directory as its working root",
          args: {name: "dir", template: "folders"},
        },
        {name: "--worktree", description: "Run the session in a new managed Git worktree"},
        {
          name: "--add-dir",
          description: "Additional directories that should be writable alongside the primary workspace",
          args: {name: "dir", template: "folders"},
        },
        {
          name: "--strict-config",
          description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
        },
      ],
      args: {
        name: "session",
        description: "Session id (UUID) or session name. UUIDs take precedence if it parses",
      },
    },
    {
      name: "fork",
      description: "Fork a previous interactive session (picker by default; use --last to fork the most recent)",
      options: [
        {name: "--last", description: "Fork the most recent session without showing the picker"},
        {
          name: "--all",
          description: "Show all sessions (disables cwd filtering and shows CWD column)",
        },
        {
          name: "--remote",
          description: "Connect the TUI to a remote app server endpoint.",
          args: {name: "addr"},
        },
        {
          name: "--remote-auth-token-env",
          description: "Name of the environment variable containing the bearer token to send to a remote app server websocket",
          args: {name: "env_var"},
        },
        {
          name: "--strict-config",
          description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
        },
        {
          name: ["-i", "--image"],
          description: "Optional image(s) to attach to the initial prompt",
          args: {name: "file", isVariadic: true, template: "filepaths"},
        },
        {name: ["-m", "--model"], description: "Model the agent should use", args: {name: "model"}},
        {name: "--oss", description: "Use open-source provider"},
        {
          name: "--local-provider",
          description: "Specify which local provider to use (lmstudio or ollama)",
          args: {name: "oss_provider", suggestions: ["lmstudio", "ollama"]},
        },
        {
          name: ["-p", "--profile"],
          description: "Layer $CODEX_HOME/<name>.config.toml on top of the base user config",
          args: {name: "config_profile_v2"},
        },
        {
          name: ["-s", "--sandbox"],
          description: "Select the sandbox policy to use when executing model-generated shell commands",
          args: {
            name: "sandbox_mode",
            suggestions: ["read-only", "workspace-write", "danger-full-access"],
          },
        },
        {
          name: "--approve-for-me",
          description: "Route approval requests through automatic review using the workspace-write sandbox",
        },
        {
          name: "--dangerously-bypass-approvals-and-sandbox",
          description: "Skip all confirmation prompts and execute commands without sandboxing",
        },
        {
          name: "--dangerously-bypass-hook-trust",
          description: "Run enabled hooks without requiring persisted hook trust for this invocation",
        },
        {
          name: ["-C", "--cd"],
          description: "Tell the agent to use the specified directory as its working root",
          args: {name: "dir", template: "folders"},
        },
        {name: "--worktree", description: "Run the session in a new managed Git worktree"},
        {
          name: "--add-dir",
          description: "Additional directories that should be writable alongside the primary workspace",
          args: {name: "dir", template: "folders"},
        },
        {
          name: ["-a", "--ask-for-approval"],
          description: "Configure when the model requires human approval before executing a command",
          args: {
            name: "approval_policy",
            suggestions: [
              {
                name: "on-request",
                description: "The model decides when to ask the user for approval",
              },
              {
                name: "never",
                description: "Never ask for user approval Execution failures are immediately returned to the model",
              },
            ],
          },
        },
        {name: "--search", description: "Enable live web search"},
        {name: "--no-alt-screen", description: "Disable alternate screen mode"},
        {
          name: "--no-daemon",
          description: "Run without the shared background server, even if it is already running",
        },
        {name: ["-V", "--version"], description: "Print version"},
      ],
      args: [
        {name: "session_id", description: "Conversation/session id (UUID)", isOptional: true},
        {name: "prompt", description: "Optional user prompt to start the session", isOptional: true},
      ],
    },
    {
      name: "cloud",
      description: "[EXPERIMENTAL] Browse tasks from Codex Cloud and apply changes locally",
      subcommands: [
        {
          name: "exec",
          description: "Submit a new Codex Cloud task without launching the TUI",
          options: [
            {
              name: "--env",
              description: "Target environment identifier (see `codex cloud` to browse)",
              args: {name: "env_id"},
            },
            {
              name: "--attempts",
              description: "Number of assistant attempts (best-of-N)",
              args: {name: "attempts"},
            },
            {
              name: "--branch",
              description: "Git branch to run in Codex Cloud (defaults to current branch)",
              args: {name: "branch"},
            },
          ],
          args: {name: "query", description: "Task prompt to run in Codex Cloud", isOptional: true},
        },
        {
          name: "status",
          description: "Show the status of a Codex Cloud task",
          args: {name: "task_id", description: "Codex Cloud task identifier to inspect"},
        },
        {
          name: "list",
          description: "List Codex Cloud tasks",
          options: [
            {
              name: "--env",
              description: "Filter tasks by environment identifier",
              args: {name: "env_id"},
            },
            {
              name: "--limit",
              description: "Maximum number of tasks to return (1-20)",
              args: {name: "n"},
            },
            {
              name: "--cursor",
              description: "Pagination cursor returned by a previous call",
              args: {name: "cursor"},
            },
            {name: "--json", description: "Emit JSON instead of plain text"},
          ],
        },
        {
          name: "apply",
          description: "Apply the diff for a Codex Cloud task locally",
          options: [{name: "--attempt", description: "Attempt number to apply (1-based)", args: {name: "n"}}],
          args: {name: "task_id", description: "Codex Cloud task identifier to apply"},
        },
        {
          name: "diff",
          description: "Show the unified diff for a Codex Cloud task",
          options: [
            {
              name: "--attempt",
              description: "Attempt number to display (1-based)",
              args: {name: "n"},
            },
          ],
          args: {name: "task_id", description: "Codex Cloud task identifier to display"},
        },
      ],
      options: [{name: ["-V", "--version"], description: "Print version"}],
    },
    {
      name: "exec-server",
      description: "[EXPERIMENTAL] Run the standalone exec-server service",
      subcommands: [
        {
          name: "forward",
          description: "Register an existing WebSocket exec-server as a remote environment",
          options: [
            {
              name: "--connect",
              description: "Destination exec-server WebSocket URL",
              args: {name: "url"},
            },
            {
              name: "--strict-config",
              description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
            },
            {
              name: "--linux-sandbox-pid-namespace",
              description: "Linux PID namespace: isolate (default) or inherit",
              args: {name: "mode"},
            },
            {
              name: "--proxy-private-ips-via-upstream",
              description: "Allow permitted private IP destinations to use the configured upstream proxy",
            },
            {
              name: "--remote",
              description: "Register this exec-server as a remote environment using the given base URL",
              args: {name: "url"},
            },
            {
              name: "--remote-transport",
              description: "Transport used for the remote executor connection",
              args: {name: "remote_transport", suggestions: ["noise", "direct"]},
            },
            {
              name: "--environment-id",
              description: "Environment id to attach to when registering remotely",
              args: {name: "id"},
            },
            {name: "--name", description: "Human-readable environment name", args: {name: "name"}},
            {
              name: "--use-agent-identity-auth",
              description: "Use Agent Identity auth from CODEX_ACCESS_TOKEN for remote registration",
            },
            {
              name: "--executor-credential-stdin",
              description: "Read rotating executor credentials from stdin; exit when the pipe closes",
            },
            {
              name: "--aws-sigv4",
              description: "Sign Direct registration and WebSocket handshake requests with AWS SigV4",
            },
            {
              name: "--aws-profile",
              description: "AWS profile used for SigV4 authentication",
              args: {name: "profile"},
            },
            {
              name: "--aws-region",
              description: "AWS signing region. Uses the SDK region chain when omitted",
              args: {name: "region"},
            },
            {name: "--aws-service", description: "AWS signing service", args: {name: "service"}},
            {
              name: "--exit-on-stdin-close",
              description: "Exit when the parent-owned standard-input pipe closes",
            },
          ],
        },
      ],
      options: [
        {
          name: "--strict-config",
          description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
        },
        {
          name: "--linux-sandbox-pid-namespace",
          description: "Linux PID namespace: isolate (default) or inherit",
          args: {name: "mode"},
        },
        {
          name: "--proxy-private-ips-via-upstream",
          description: "Allow permitted private IP destinations to use the configured upstream proxy",
        },
        {
          name: "--concurrent-requests",
          description: "Maximum number of requests to process concurrently on each connection",
          args: {name: "count"},
        },
        {
          name: "--listen",
          description: "Transport endpoint URL. Supported values: `ws://IP:PORT` (default), `stdio`, `stdio://`",
          args: {name: "url"},
        },
        {
          name: "--ws-auth",
          description: "Authentication mode for incoming WebSocket connections",
          args: {name: "mode", suggestions: ["capability-token", "signed-bearer-token"]},
        },
        {
          name: "--ws-token-file",
          description: "Absolute path to the capability-token file",
          args: {name: "path", template: "filepaths"},
        },
        {
          name: "--ws-token-sha256",
          description: "Hex-encoded SHA-256 digest of the capability token",
          args: {name: "hex"},
        },
        {
          name: "--ws-shared-secret-file",
          description: "Absolute path to the shared secret file for signed JWT bearer tokens",
          args: {name: "path", template: "filepaths"},
        },
        {
          name: "--ws-issuer",
          description: "Expected issuer for signed JWT bearer tokens",
          args: {name: "issuer"},
        },
        {
          name: "--ws-audience",
          description: "Expected audience for signed JWT bearer tokens",
          args: {name: "audience"},
        },
        {
          name: "--ws-max-clock-skew-seconds",
          description: "Maximum clock skew when validating signed JWT bearer tokens",
          args: {name: "seconds"},
        },
        {
          name: "--remote",
          description: "Register this exec-server as a remote environment using the given base URL",
          args: {name: "url"},
        },
        {
          name: "--remote-transport",
          description: "Transport used for the remote executor connection",
          args: {name: "remote_transport", suggestions: ["noise", "direct"]},
        },
        {
          name: "--environment-id",
          description: "Environment id to attach to when registering remotely",
          args: {name: "id"},
        },
        {name: "--name", description: "Human-readable environment name", args: {name: "name"}},
        {
          name: "--use-agent-identity-auth",
          description: "Use Agent Identity auth from CODEX_ACCESS_TOKEN for remote registration",
        },
        {
          name: "--executor-credential-stdin",
          description: "Read rotating executor credentials from stdin; exit when the pipe closes",
        },
        {
          name: "--aws-sigv4",
          description: "Sign Direct registration and WebSocket handshake requests with AWS SigV4",
        },
        {
          name: "--aws-profile",
          description: "AWS profile used for SigV4 authentication",
          args: {name: "profile"},
        },
        {
          name: "--aws-region",
          description: "AWS signing region. Uses the SDK region chain when omitted",
          args: {name: "region"},
        },
        {name: "--aws-service", description: "AWS signing service", args: {name: "service"}},
        {
          name: "--exit-on-stdin-close",
          description: "Exit when the parent-owned standard-input pipe closes",
        },
      ],
    },
    {
      name: "features",
      description: "Inspect feature flags",
      subcommands: [
        {name: "list", description: "List known features with their stage and effective state"},
        {
          name: "enable",
          description: "Enable a feature in config.toml",
          args: {name: "feature", description: "Feature key to update (for example: unified_exec)"},
        },
        {
          name: "disable",
          description: "Disable a feature in config.toml",
          args: {name: "feature", description: "Feature key to update (for example: unified_exec)"},
        },
      ],
    },
  ],
  options: [
    {
      name: ["-c", "--config"],
      description: "Override a configuration value that would otherwise be loaded from `~/.codex/config.toml`",
      args: {name: "key=value"},
      isPersistent: true,
    },
    {
      name: "--enable",
      description: "Enable a feature (repeatable). Equivalent to `-c features.<name>=true`",
      args: {name: "feature"},
      isRepeatable: true,
      isPersistent: true,
    },
    {
      name: "--disable",
      description: "Disable a feature (repeatable). Equivalent to `-c features.<name>=false`",
      args: {name: "feature"},
      isRepeatable: true,
      isPersistent: true,
    },
    {
      name: "--remote",
      description: "Connect the TUI to a remote app server endpoint.",
      args: {name: "addr"},
    },
    {
      name: "--remote-auth-token-env",
      description: "Name of the environment variable containing the bearer token to send to a remote app server websocket",
      args: {name: "env_var"},
    },
    {
      name: "--strict-config",
      description: "Error out when config.toml contains fields that are not recognized by this version of Codex",
    },
    {
      name: ["-i", "--image"],
      description: "Optional image(s) to attach to the initial prompt",
      args: {name: "file", isVariadic: true, template: "filepaths"},
    },
    {name: ["-m", "--model"], description: "Model the agent should use", args: {name: "model"}},
    {name: "--oss", description: "Use open-source provider"},
    {
      name: "--local-provider",
      description: "Specify which local provider to use (lmstudio or ollama)",
      args: {name: "oss_provider", suggestions: ["lmstudio", "ollama"]},
    },
    {
      name: ["-p", "--profile"],
      description: "Layer $CODEX_HOME/<name>.config.toml on top of the base user config",
      args: {name: "config_profile_v2"},
    },
    {
      name: ["-s", "--sandbox"],
      description: "Select the sandbox policy to use when executing model-generated shell commands",
      args: {name: "sandbox_mode", suggestions: ["read-only", "workspace-write", "danger-full-access"]},
    },
    {
      name: "--approve-for-me",
      description: "Route approval requests through automatic review using the workspace-write sandbox",
    },
    {
      name: "--dangerously-bypass-approvals-and-sandbox",
      description: "Skip all confirmation prompts and execute commands without sandboxing",
    },
    {
      name: "--dangerously-bypass-hook-trust",
      description: "Run enabled hooks without requiring persisted hook trust for this invocation",
    },
    {
      name: ["-C", "--cd"],
      description: "Tell the agent to use the specified directory as its working root",
      args: {name: "dir", template: "folders"},
    },
    {name: "--worktree", description: "Run the session in a new managed Git worktree"},
    {
      name: "--add-dir",
      description: "Additional directories that should be writable alongside the primary workspace",
      args: {name: "dir", template: "folders"},
    },
    {
      name: ["-a", "--ask-for-approval"],
      description: "Configure when the model requires human approval before executing a command",
      args: {
        name: "approval_policy",
        suggestions: [
          {name: "on-request", description: "The model decides when to ask the user for approval"},
          {
            name: "never",
            description: "Never ask for user approval Execution failures are immediately returned to the model",
          },
        ],
      },
    },
    {name: "--search", description: "Enable live web search"},
    {name: "--no-alt-screen", description: "Disable alternate screen mode"},
    {
      name: "--no-daemon",
      description: "Run without the shared background server, even if it is already running",
    },
    {
      name: ["-h", "--help"],
      description: "Print help (see a summary with '-h')",
      isPersistent: true,
    },
    {name: ["-V", "--version"], description: "Print version"},
  ],
  args: {name: "prompt", description: "Optional user prompt to start the session", isOptional: true},
};
