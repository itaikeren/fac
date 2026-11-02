// From cursor-agent --help and the help of each subcommand, Cursor Agent 2026.10.22-27dcf5f, 2026-11-02.
export default {
  name: "cursor-agent",
  subcommands: [
    {name: "persist", description: "Start or manage a session that survives terminal disconnects"},
    {name: "install-shell-integration", description: "Install shell integration to ~/.zshrc"},
    {name: "uninstall-shell-integration", description: "Remove shell integration from ~/.zshrc"},
    {
      name: "login",
      description: "Authenticate with Cursor. Set NO_OPEN_BROWSER to disable browser opening.",
    },
    {name: "logout", description: "Sign out and clear stored authentication"},
    {
      name: "mcp",
      description: "Manage MCP servers",
      subcommands: [
        {
          name: "login",
          description: "Authenticate with an MCP server configured in .cursor/mcp.json or ~/.cursor/mcp.json",
          args: {name: "identifier"},
        },
        {name: "list", description: "List configured MCP servers and their status"},
        {
          name: "list-tools",
          description: "List available tools and their argument names for a specific MCP",
          args: {name: "identifier"},
        },
        {
          name: "enable",
          description: "Add an MCP server to the local approved list",
          args: {name: "identifier"},
        },
        {
          name: "disable",
          description: "Disable an MCP server (it will not be loaded or prompted for approval)",
          args: {name: "identifier"},
        },
      ],
    },
    {
      name: "plugin",
      description: "Manage plugins and plugin marketplaces",
      subcommands: [
        {
          name: "marketplace",
          description: "Manage plugin marketplaces",
          subcommands: [
            {
              name: "add",
              description: "Add a plugin marketplace from a git repository URL",
              options: [
                {
                  name: "--git-ref",
                  description: "Branch, tag, or commit to index (defaults to the default branch)",
                  args: {name: "ref"},
                },
              ],
              args: {name: "gitUrl"},
            },
            {
              name: "list",
              description: "List plugin marketplaces visible to this account",
              options: [
                {
                  name: "--format",
                  description: "Output format (choices: \"text\", \"json\", default: \"text\")",
                  args: {name: "format", suggestions: ["text", "json"]},
                },
              ],
            },
            {
              name: "remove",
              description: "Remove a plugin marketplace by name or git repository URL",
              args: {name: "nameOrUrl"},
            },
            {
              name: "update",
              description: "Re-index a plugin marketplace from its git repository",
              args: {name: "nameOrUrl"},
            },
          ],
        },
      ],
    },
    {
      name: "worker",
      description: "Run a self-hosted Cloud Agent worker that connects to Cursor and executes agent tool calls on this machine.",
      subcommands: [
        {
          name: "start",
          description: "Start the worker and connect to Cursor",
          options: [{name: "--verbose", description: "Enable verbose worker startup logs (includes debug)"}],
        },
        {
          name: "controller",
          description: "Run one of two explicit modes:",
          options: [
            {
              name: "--spawn",
              description: "Spawn hook to exec once after a successful claim (or once per missing warm worker).",
              args: {name: "path", template: "filepaths"},
            },
            {
              name: "--pool",
              description: "Claim mode: list pending requests with GET ?pool=<name> (exact, case-sensitive); repeatable: --pool...",
              args: {name: "name", suggestions: ["exact", "case-sensitive"]},
            },
            {
              name: "--all-pools",
              description: "Omit the pool query param so pending-requests is team-wide. Mutually exclusive with --pool. Not allowed in warm mode.",
            },
            {
              name: "--warm-idle",
              description: "Warm-pool mode: keep <count> idle workers connected in each --pool by pre-spawning unclaimed workers.",
              args: {name: "count"},
            },
            {
              name: "--repository",
              description: "Filter pending requests by repository URL.",
              args: {name: "url"},
            },
            {
              name: "--session-token",
              description: "Claim mode only: give the spawn hook CURSOR_AUTH_TOKEN, a session token that serves only its claim...",
            },
            {
              name: "--identity-audience",
              description: "Claim mode only: ask each claim for a short-lived signed identity token bound to <aud> and a fresh...",
              args: {name: "aud"},
            },
          ],
        },
        {
          name: "debug",
          description: "Run private worker preflight diagnostics for auth, privacy, routing, and visibility",
          options: [{name: "--json", description: "Output debug report as JSON (default: false)"}],
        },
      ],
      options: [
        {
          name: "--auth-token-file",
          description: "Path to a file containing the worker auth token. Intended for operator-managed Secret mounts.",
          args: {name: "path", template: "filepaths"},
        },
        {
          name: "--worker-dir",
          description: "Workspace root to expose to agents (repeatable; first value is the assignment identity and defaults...",
          args: {name: "path", template: "folders"},
          isRepeatable: true,
        },
        {
          name: "--management-addr",
          description: "Listen address for an optional HTTP server: GET /healthz, /readyz (Kubernetes probes), and /metrics...",
          args: {name: "address"},
        },
        {
          name: "--label",
          description: "Add a label to the worker (format: key=value, can be used multiple times). Cannot be used with --labels-file.",
          args: {name: "key=value"},
          isRepeatable: true,
        },
        {
          name: "--labels-file",
          description: "Path to a JSON or TOML file containing labels.",
          args: {name: "path", template: "filepaths"},
        },
        {
          name: "--idle-release-timeout",
          description: "Seconds the worker may stay connected after becoming idle before it is released.",
          args: {name: "seconds"},
        },
        {
          name: "--pool",
          description: "Join a team Self-Hosted Pool instead of registering a personal My Machines worker.",
          args: {name: "name", isOptional: true},
        },
        {
          name: "--mint-github-token",
          description: "Receive short-lived GitHub tokens during claimed runs.",
        },
        {
          name: "--clone-git-repos",
          description: "Check out the claimed agent's Git repositories in this worker's workspace: a clean checkout of the...",
        },
        {
          name: "--sync-dashboard-secrets",
          description: "Receive eligible dashboard Cloud Agent secrets as environment variables during claimed runs.",
        },
        {
          name: "--on-session-start",
          description: "Add a worker-host sessionStart command after a pool claim. The primary configuration path remains hooks.json.",
          args: {name: "command"},
        },
        {
          name: "--on-session-end",
          description: "Add a worker-host sessionEnd command before pool claim teardown. The primary configuration path remains hooks.json.",
          args: {name: "command"},
        },
        {
          name: "--mcp-tool-timeout",
          description: "How long a tool call to a stdio MCP server this worker runs may take, in seconds (repeatable)....",
          args: {name: "server[/tool]=seconds"},
          isRepeatable: true,
        },
        {
          name: "--identity-socket",
          description: "Expose a per-claim OIDC identity socket to claimed agents (published as CURSOR_AGENT_SOCKET in agent shells).",
        },
        {
          name: "--computer-use",
          description: "Let claimed cloud agents drive this machine's desktop (screenshot, click, type).",
        },
        {
          name: "--display",
          description: "Existing X11 display to require for --computer-use (for example :0).",
          args: {name: "display"},
        },
        {
          name: "--share-desktop",
          description: "Let authorized viewers watch this worker's desktop through Cursor (\"Show desktop\").",
          args: {name: "mode", isOptional: true, suggestions: ["view", "view_and_control"]},
        },
        {
          name: "--connect-workspace",
          description: "Let Cursor connect to the live workspace (files, changes, terminal, ports) of agents this worker...",
        },
        {name: "--single-use", description: "Same as --pool (legacy alias)."},
        {
          name: "--pool-name",
          description: "Deprecated alias for --pool <name>. Requires --pool or --single-use. (env: CURSOR_WORKER_POOL_NAME)",
          args: {name: "name"},
        },
        {
          name: "--name",
          description: "Custom display name for this worker. Defaults to the machine hostname. (env: CURSOR_WORKER_NAME)",
          args: {name: "name"},
        },
        {
          name: "--data-dir",
          description: "Base directory for logs, artifacts, and recording data.",
          args: {name: "path", template: "folders"},
        },
        {
          name: "--wait",
          description: "Wait for another worker using this data directory to stop.",
          args: {name: "seconds", isOptional: true},
        },
        {name: "--debug", description: "Print worker debug diagnostics before starting bridge mode"},
      ],
    },
    {
      name: ["status", "whoami"],
      description: "View authentication status",
      options: [
        {
          name: "--format",
          description: "Output format (choices: \"text\", \"json\", default: \"text\")",
          args: {name: "format", suggestions: ["text", "json"]},
        },
      ],
    },
    {name: "models", description: "List available models for this account"},
    {
      name: "team",
      description: "View or switch the active team for this account",
      subcommands: [
        {
          name: "list",
          description: "List the teams this account belongs to",
          options: [
            {
              name: "--format",
              description: "Output format (choices: \"text\", \"json\", default: \"text\")",
              args: {name: "format", suggestions: ["text", "json"]},
            },
          ],
        },
        {
          name: "switch",
          description: "Switch the active team (by id or name, or interactively); scopes models and settings to that team",
          options: [
            {
              name: "--format",
              description: "Output format (choices: \"text\", \"json\", default: \"text\")",
              args: {name: "format", suggestions: ["text", "json"]},
            },
          ],
          args: {name: "team", isOptional: true},
        },
      ],
    },
    {
      name: "bedrock",
      description: "Configure AWS Bedrock usage for CLI",
      subcommands: [
        {
          name: "configure",
          description: "Configure AWS Bedrock settings (credentials, region, test model)",
          options: [
            {name: "--access-key", description: "AWS Access Key ID", args: {name: "key"}},
            {name: "--secret-key", description: "AWS Secret Access Key", args: {name: "key"}},
            {name: "--from-env", description: "Read credentials from AWS_* environment variables"},
            {name: "--region", description: "AWS region (e.g. us-east-1)", args: {name: "region"}},
            {
              name: "--test-model",
              description: "Bedrock model ID used for validation",
              args: {name: "model"},
            },
          ],
        },
        {name: "enable", description: "Enable Bedrock mode (requires stored credentials)"},
        {name: "status", description: "Show Bedrock configuration and team-role availability"},
        {name: "test", description: "Validate stored Bedrock credentials with the backend"},
        {
          name: "clear",
          description: "Clear Bedrock credentials from secure storage and disable mode",
        },
        {name: "disable", description: "Disable Bedrock mode but keep stored credentials"},
      ],
    },
    {
      name: "about",
      description: "Display version, system, and account information",
      options: [
        {
          name: "--format",
          description: "Output format (choices: \"text\", \"json\", default: \"text\")",
          args: {name: "format", suggestions: ["text", "json"]},
        },
      ],
    },
    {name: "update", description: "Update Cursor Agent to the latest version"},
    {name: "create-chat", description: "Create a new empty chat and return its ID"},
    {
      name: ["generate-rule", "rule"],
      description: "Generate a new Cursor rule with interactive prompts",
    },
    {
      name: "agent",
      description: "Start the Cursor Agent",
      args: {name: "prompt", description: "Initial prompt for the agent"},
    },
    {name: "ls", description: "Resume a chat session"},
    {name: "resume", description: "Resume the latest chat session"},
  ],
  options: [
    {name: ["-v", "--version"], description: "Output the version number"},
    {
      name: "--api-key",
      description: "API key for authentication (can also use CURSOR_API_KEY env var)",
      args: {name: "key"},
    },
    {
      name: ["-H", "--header"],
      description: "Add custom header to agent requests (format: 'Name: Value', can be used multiple times)",
      args: {name: "header"},
      isRepeatable: true,
    },
    {
      name: ["-e", "--endpoint"],
      description: "Target API endpoint URL (can also use CURSOR_API_ENDPOINT env var) (default...",
      args: {name: "url"},
    },
    {
      name: ["-p", "--print"],
      description: "Print responses to console (for scripts or non-interactive use).",
    },
    {
      name: "--output-format",
      description: "Output format (only works with --print): text | json | stream-json (default: \"text\")",
      args: {name: "format", suggestions: ["json", "stream-json"]},
    },
    {
      name: "--stream-partial-output",
      description: "Stream partial output as individual text deltas (only works with --print and stream-json format) (default: false)",
    },
    {
      name: "--mode",
      description: "Start in the given execution mode. plan: read-only/planning (analyze, propose plans, no edits)....",
      args: {name: "mode", suggestions: ["plan", "ask"]},
    },
    {
      name: "--plan",
      description: "Start in plan mode (shorthand for --mode=plan). (default: false)",
    },
    {
      name: "--resume",
      description: "Select a session to resume (default: false)",
      args: {name: "chatId", isOptional: true},
    },
    {name: "--continue", description: "Continue previous session (default: false)"},
    {
      name: "--model",
      description: "Model to use (e.g., gpt-5, sonnet-4-thinking).",
      args: {name: "model"},
    },
    {name: "--list-models", description: "List available models and exit (default: false)"},
    {
      name: ["-f", "--force"],
      description: "Force allow commands unless explicitly denied (default: false)",
    },
    {name: "--yolo", description: "Alias for --force (Run Everything) (default: false)"},
    {
      name: "--auto-review",
      description: "Use Auto-review (Smart Auto): a server classifier auto-runs safe tool calls and prompts for the rest (default: false)",
    },
    {
      name: "--sandbox",
      description: "Explicitly enable or disable sandbox mode (overrides config) (choices: \"enabled\", \"disabled\")",
      args: {name: "mode", suggestions: ["enabled", "disabled"]},
    },
    {name: "--approve-mcps", description: "Automatically approve all MCP servers (default: false)"},
    {name: "--trust", description: "Trust the current workspace without prompting (default: false)"},
    {
      name: "--workspace",
      description: "Workspace directory or saved workspace name to use (defaults to current working directory)",
      args: {name: "path-or-name"},
    },
    {
      name: "--add-dir",
      description: "Add an additional workspace root directory (can be specified multiple times)",
      args: {name: "path", template: "folders"},
    },
    {
      name: "--plugin-dir",
      description: "Load a local plugin directory (can be specified multiple times)",
      args: {name: "path", template: "folders"},
    },
    {
      name: ["-w", "--worktree"],
      description: "Start in an isolated git worktree at ~/.cursor/worktrees/<reponame>/<name>. If omitted, a name is generated.",
      args: {name: "name", isOptional: true},
    },
    {
      name: "--worktree-base",
      description: "Branch or ref to base the new worktree on (default: current HEAD)",
      args: {name: "branch"},
    },
    {
      name: "--skip-worktree-setup",
      description: "Skip running worktree setup scripts from .cursor/worktrees.json (default: false)",
    },
    {name: ["-h", "--help"], description: "Display help for command", isPersistent: true},
  ],
  args: {name: "prompt", description: "Initial prompt for the agent"},
};
