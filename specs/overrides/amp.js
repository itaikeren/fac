// From amp --help and the help of each subcommand, Amp CLI 0.0.1793606440-g4c7784, 2026-11-02.
export default {
  name: "amp",
  subcommands: [
    {name: "version", description: "Print the version number and exit"},
    {
      name: "logout",
      description: "Remove a saved Amp account",
      options: [{name: "--all", description: "Remove all saved CLI accounts across all servers"}],
    },
    {
      name: "account",
      description: "Manage saved Amp accounts",
      subcommands: [
        {
          name: "list",
          description: "List saved accounts",
          options: [{name: "--json", description: "Output as JSON"}],
        },
        {name: "switch", description: "Choose the account for future CLI commands"},
      ],
    },
    {name: "login", description: "Log in to Amp"},
    {
      name: "orb",
      description: "Commands for Amp-managed orbs",
      subcommands: [
        {
          name: "provider",
          description: "Run self-hosted orbs from inside your network by pulling lifecycle jobs from Amp over HTTPS",
          options: [
            {
              name: "--token",
              description: "Orb provider token from workspace settings (aop_live_…); defaults to AMP_ORB_PROVIDER_TOKEN",
              args: {name: "token"},
            },
            {
              name: "--allow-multiple",
              description: "Run beside other orb providers for this token; only when your backend shares state across them",
            },
            {name: "--amp-url", description: "Amp server URL", args: {name: "url"}},
          ],
          args: {name: "script", description: "Executable handling lifecycle jobs using ORB_OPERATION"},
        },
        {name: "bootstrap", description: "Connect this orb to its thread actor"},
        {
          name: "system-metrics",
          description: "Print historical orb system metrics for a thread URL or ID",
        },
        {
          name: "portal",
          description: "Register a portal for an existing process and print its URL; use \"amp orb service start\" for long-lived servers",
          subcommands: [{name: "access", description: "Set portal access"}],
          options: [
            {
              name: "--thread",
              description: "Thread URL or ID (defaults to AMP_THREAD_ID)",
              args: {name: "thread"},
            },
            {
              name: "--domain",
              description: "Portal domain used to recognize portal URL targets and to build the fallback URL when Amp cannot be...",
              args: {name: "domain"},
            },
            {
              name: "--name",
              description: "Portal manifest name; writes .amp/portals/<name>.json (defaults to portal-<port>)",
              args: {name: "manifest-name"},
            },
            {
              name: "--title",
              description: "Portal tab title; writes the portal manifest .amp/portals/<name>.json (defaults to the manifest name)",
              args: {name: "title"},
            },
            {
              name: "--description",
              description: "Portal tab description shown as a note next to the link; written to the portal manifest",
              args: {name: "description"},
            },
            {
              name: "--hostname",
              description: "Set the portal's public hostname (a full managed hostname like <name>--<namespace>.onamp.dev or a...",
              args: {name: "hostname"},
            },
            {
              name: "--take-over",
              description: "Move a hostname from another thread portal; requires --hostname",
            },
          ],
        },
        {
          name: "service",
          description: "[alias: services] Manage long-lived services (dev servers, watchers) as supervised processes that...",
          subcommands: [
            {
              name: "ensure",
              description: "Ensure every service declared in .amp/services.yaml is running and exposed at its portal URLs",
              options: [{name: "--json", description: "Print machine-readable JSON results"}],
            },
            {
              name: "start",
              description: "Start an undeclared ad-hoc supervised service, e.g.: amp orb service start preview --command 'pnpm dev' --portal",
              options: [
                {
                  name: "--command",
                  description: "Shell command to run; it runs with $PORT set (and $PUBLIC_URL when --portal) and should listen on $PORT",
                  args: {name: "command"},
                },
                {
                  name: "--cwd",
                  description: "Working directory for the command (defaults to the current directory)",
                  args: {name: "dir", template: "folders"},
                },
                {
                  name: "--port",
                  description: "Local port for $PORT (defaults to the running service’s current port, else a free port)",
                  args: {name: "port"},
                },
                {
                  name: "--portal",
                  description: "Expose $PORT at a public portal URL: waits for it to listen, autodetects HTTP vs HTTPS, writes the...",
                },
                {
                  name: "--title",
                  description: "Portal tab title (defaults to the service name; requires --portal)",
                  args: {name: "title"},
                },
                {
                  name: "--description",
                  description: "Portal tab description shown as a note next to the link (requires --portal)",
                  args: {name: "description"},
                },
              ],
            },
            {
              name: "restart",
              description: "Restart a declared service from .amp/services.yaml or reload an existing ad-hoc service",
            },
            {name: "stop", description: "Stop a service"},
            {name: "status", description: "Show supervisor status for a service"},
            {
              name: "logs",
              description: "Show recent logs for a service",
              options: [
                {
                  name: ["-n", "--lines"],
                  description: "Number of log lines to show",
                  args: {name: "count"},
                },
              ],
            },
            {name: "list", description: "List orb services"},
          ],
        },
        {
          name: "simulator",
          description: "Work with the iOS Simulator on macOS",
          subcommands: [
            {
              name: "serve",
              description: "Serve one booted iOS Simulator as an interactive web page",
              options: [
                {
                  name: "--port",
                  description: "Port to listen on (defaults to $PORT, which Amp sets for a declared service)",
                  args: {name: "port"},
                },
                {
                  name: "--device",
                  description: "Simulator name or UDID (defaults to the booted iPhone)",
                  args: {name: "device"},
                },
                {
                  name: "--runtime",
                  description: "Runtime version such as 26.2, when a name has several",
                  args: {name: "runtime"},
                },
              ],
            },
          ],
        },
        {
          name: "desktop",
          description: "Work with the orb Desktop shown in the Desktop tab of the thread",
          subcommands: [
            {
              name: "ensure",
              description: "Start the Desktop and its Google Chrome if needed and print the Chrome DevTools endpoint for agent-browser --cdp",
              options: [{name: "--json", description: "Print machine-readable JSON"}],
            },
          ],
        },
        {
          name: "restart-processes",
          description: "Refresh environment variables and restart all processes in an orb thread",
          options: [
            {
              name: "--thread",
              description: "Thread URL (defaults to AMP_THREAD_ID)",
              args: {name: "thread-url"},
            },
          ],
        },
        {
          name: "id-token",
          description: "Mint a short-lived OIDC token with a caller-chosen audience and print it to stdout",
          options: [
            {
              name: "--audience",
              description: "The aud claim of the minted token",
              args: {name: "audience"},
            },
            {
              name: "--ttl-seconds",
              description: "Token lifetime in seconds (60-3600)",
              args: {name: "seconds"},
            },
            {
              name: "--subject-scope",
              description: "Subject scope of the minted token (project, thread, user, or workspace; default: thread)",
              args: {name: "scope"},
            },
          ],
        },
        {
          name: "redact-output",
          description: "Copy stdin to stdout with secrets redacted: known token formats, the thread's secret values, and...",
          options: [
            {
              name: "--thread",
              description: "Thread URL (defaults to AMP_THREAD_ID)",
              args: {name: "thread-url"},
            },
          ],
        },
      ],
    },
    {
      name: "portal",
      description: "Commands for accessing Amp portal URLs",
      subcommands: [
        {
          name: "login-url",
          description: "Mint a single-use login URL (valid ~60s) that signs a non-browser client into an Amp portal URL...",
        },
      ],
    },
    {
      name: "net",
      description: "Amp Net: send an orb’s outbound traffic through an exit node on your own machine (for example a residential connection)",
      subcommands: [
        {
          name: "exit-node",
          description: "Run this machine as an exit node for your orbs: it appears in your runner list and any orb thread...",
          options: [
            {
              name: "--id",
              description: "ID and name for this exit node (hostname-like); lets several exit nodes run on one machine.",
              args: {name: "id"},
            },
            {
              name: "--thread",
              description: "Only serve this thread (ID or URL); repeat to allow several. Threads you do not own are never served.",
              args: {name: "thread"},
            },
            {
              name: "--verbose",
              description: "Log every connection the orbs open through this machine",
            },
            {
              name: "--portal-domain",
              description: "Amp portal domain (defaults to AMP_PORTAL_DOMAIN)",
              args: {name: "domain"},
            },
          ],
        },
        {
          name: "route",
          description: "In an orb: send this orb’s traffic through one of your exit nodes (by ID or hostname).",
          options: [
            {
              name: "--system",
              description: "Stop routing through an exit node and use the orb’s own network",
            },
            {
              name: "--wait",
              description: "How long to wait for the exit node to connect before giving up (0 to return immediately)",
              args: {name: "seconds"},
            },
          ],
        },
        {
          name: "status",
          description: "In an orb: show whether Amp Net is configured and which exit node is connected",
          options: [{name: "--json", description: "Print machine-readable JSON"}],
        },
        {
          name: "ping",
          description: "In an orb: measure the round trip from this orb to the connected exit node",
          options: [{name: "--count", description: "Number of pings to send", args: {name: "n"}}],
        },
        {
          name: "run",
          description: "In an orb: run one command with its network traffic sent through the exit node, without changing orb-wide configuration",
          args: {
            name: "command",
            description: "Command and arguments; use `--` before commands that take options",
          },
        },
      ],
    },
    {
      name: "profile",
      description: "Amp Profiles: the orb Desktop Chrome’s sign-ins, saved passwords, and site data, saved by Amp so...",
      subcommands: [
        {name: "list", description: "List your profiles and mark the one this orb uses"},
        {name: "create", description: "Create an empty profile"},
        {name: "rename", description: "Rename a profile; threads using it keep using it"},
        {
          name: "delete",
          description: "[alias: rm] Delete a profile with its saved sign-ins and site data; threads using it switch to a clean profile",
          options: [{name: ["-y", "--yes"], description: "Delete without asking"}],
        },
        {
          name: "use",
          description: "Use a profile in this orb’s Chrome; Chrome restarts with it.",
          options: [
            {
              name: "--thread",
              description: "Thread to change (defaults to this orb’s thread)",
              args: {name: "thread"},
            },
          ],
        },
        {
          name: "none",
          description: "Stop using a profile; Chrome restarts with a clean profile",
          options: [
            {
              name: "--thread",
              description: "Thread to change (defaults to this orb’s thread)",
              args: {name: "thread"},
            },
          ],
        },
        {
          name: "status",
          description: "Show which profile this orb uses and when it was last saved",
          options: [
            {
              name: "--thread",
              description: "Thread to show (defaults to this orb’s thread)",
              args: {name: "thread"},
            },
          ],
        },
      ],
    },
    {
      name: "runner",
      description: "Inspect and change the amp --no-tui runners on this machine",
      subcommands: [
        {
          name: "list",
          description: "List the running runners on this machine and the directories they serve",
          options: [{name: "--json", description: "output as JSON"}],
        },
        {
          name: "dirs",
          description: "Change the directories a running runner serves, without restarting it",
          subcommands: [
            {
              name: "add",
              description: "Start serving one or more directories; they are served again after the runner restarts",
              options: [
                {
                  name: "--runner-id",
                  description: "The runner to change; required when several are running",
                  args: {name: "id"},
                },
              ],
            },
            {
              name: "remove",
              description: "Stop serving one or more directories added with `amp runner dirs add`",
              options: [
                {
                  name: "--runner-id",
                  description: "The runner to change; required when several are running",
                  args: {name: "id"},
                },
              ],
            },
            {
              name: "list",
              description: "List the directories a running runner serves",
              options: [
                {
                  name: "--runner-id",
                  description: "The runner to inspect; required when several are running",
                  args: {name: "id"},
                },
              ],
            },
          ],
        },
        {
          name: "desktop",
          description: "Inspect or allow desktop sharing for --no-tui --desktop runners",
          subcommands: [
            {
              name: "status",
              description: "Show desktop sharing readiness (macOS permissions, Linux programs)",
            },
            {name: "allow", description: "Request Screen Recording or Accessibility permission"},
          ],
        },
      ],
    },
    {
      name: "clone",
      description: "Clone an Amp-hosted Project, Personal Plugins or Skills repository, or Workspace Plugins or Skills repository",
      options: [
        {
          name: "--no-git-setup",
          description: "Do not configure the Git credential helper automatically (or set AMP_NO_GIT_SETUP=1)",
        },
      ],
    },
    {
      name: "git-setup",
      description: "Configure the Amp Git credential helper",
      options: [{name: "--uninstall", description: "Remove only the Amp Git credential helper configuration"}],
    },
    {
      name: "files",
      description: "Download and unpublish Amp files",
      subcommands: [
        {
          name: "get",
          description: "Download a private Amp attachment",
          options: [
            {
              name: ["-o", "--output"],
              description: "Write the attachment to this file, or into this directory under its default name",
              args: {name: "path", template: "filepaths"},
            },
          ],
        },
        {
          name: "delete",
          description: "Unpublish a file you published, so its public URL stops working",
        },
      ],
    },
    {
      name: "top",
      description: "Show live active threads",
      options: [
        {
          name: "--stream-jsonl",
          description: "Stream a JSON line whenever the thread list changes (output schema is EXPERIMENTAL)",
        },
      ],
    },
    {name: "last", description: "[alias: l] Continue the last thread"},
    {
      name: "report-bug",
      description: "[alias: report, diagnostics] Report a bug with diagnostics from the amp --no-tui runner in this directory",
    },
    {
      name: "threads",
      description: "[alias: t, thread] Manage threads",
      subcommands: [
        {name: "new", description: "[alias: n] Create a new thread"},
        {
          name: "continue",
          description: "[alias: c] Continue an existing thread",
          options: [
            {
              name: "--last",
              description: "Continue the last thread in this checkout for the current mode, preferring this terminal",
            },
            {
              name: "--pick",
              description: "Pick a thread interactively from a list (DEPRECATED: picker is now the default)",
            },
          ],
        },
        {
          name: "list",
          description: "[alias: l, ls] List all threads",
          options: [
            {name: "--include-archived", description: "Include archived threads in the list"},
            {
              name: "--installation-id",
              description: "Only list threads for a specific installation ID",
              args: {name: "installationID"},
            },
            {
              name: "--limit",
              description: "Maximum number of threads to return",
              args: {name: "number"},
            },
            {name: "--offset", description: "Number of threads to skip", args: {name: "number"}},
            {name: "--json", description: "Output as JSON"},
          ],
        },
        {
          name: "usage",
          description: "Show usage information for a thread",
          options: [
            {name: "--details", description: "Show detailed credit, token, model, and tool usage"},
            {
              name: "--start",
              description: "Start of the usage range as an ISO 8601 timestamp",
              args: {name: "timestamp"},
            },
            {
              name: "--end",
              description: "Exclusive end of the usage range as an ISO 8601 timestamp",
              args: {name: "timestamp"},
            },
          ],
        },
        {
          name: "context",
          description: "[alias: analyze-context] Analyze the current context for a thread",
        },
        {
          name: "visibility",
          description: "[alias: v] Show or set default visibility for this repository",
        },
        {
          name: "search",
          description: "[alias: find] Search threads",
          options: [
            {
              name: ["-n", "--limit"],
              description: "Maximum number of threads to return",
              args: {name: "number"},
            },
            {name: "--json", description: "Output as JSON"},
          ],
        },
        {name: "color", description: "Set a thread color"},
        {name: "label", description: "Add labels to a thread"},
        {
          name: "share",
          description: "[alias: s] Share a thread",
          subcommands: [
            {
              name: "multiplayer",
              description: "Turn multiplayer on or off",
              subcommands: [
                {name: "on", description: "Turn multiplayer on for the default duration (1 week)"},
                {name: "off", description: "Turn multiplayer off"},
                {
                  name: "ttl",
                  description: "Turn multiplayer on for a specific duration",
                  options: [
                    {name: "--minutes", description: "Duration in minutes", args: {name: "minutes"}},
                    {name: "--hours", description: "Duration in hours", args: {name: "hours"}},
                    {name: "--days", description: "Duration in days", args: {name: "days"}},
                    {name: "--weeks", description: "Duration in weeks", args: {name: "weeks"}},
                  ],
                },
              ],
            },
          ],
        },
        {
          name: "import",
          description: "Import conversations from other coding agents as read-only Amp threads",
          subcommands: [
            {
              name: "detect",
              description: "Show which coding agents have conversations on this machine",
              options: [{name: "--json", description: "Output as JSON"}],
            },
            {
              name: "codex",
              description: "Import Codex conversations from this machine",
              options: [
                {
                  name: "--all",
                  description: "Import every recent Codex conversation on this machine",
                },
                {
                  name: "--max-age",
                  description: "With --all, only import conversations updated within this many days; 0 for no limit",
                  args: {name: "days"},
                },
                {
                  name: "--limit",
                  description: "Maximum number of conversations to import with --all",
                  args: {name: "n"},
                },
                {
                  name: "--dry-run",
                  description: "Read and convert conversations without creating Amp threads",
                },
                {
                  name: "--expect-user-email",
                  description: "Require the Amp CLI to be signed in as this email",
                  args: {name: "email"},
                },
                {
                  name: "--recheck",
                  description: "With --all, ignore the local record of past imports and ask the server again (use after deleting imported threads)",
                },
                {
                  name: "--reimport",
                  description: "Import conversations again even if already imported, replacing the messages of their existing Amp threads",
                },
                {
                  name: "--json",
                  description: "Write progress as one JSON object per line (used by the Amp Mac app to run imports in the background)",
                },
                {
                  name: "--codex-dir",
                  description: "Codex sessions directory (defaults to $CODEX_HOME/sessions or ~/.codex/sessions)",
                  args: {name: "path", template: "folders"},
                },
              ],
            },
            {
              name: "claude",
              description: "[alias: claude-code] Import Claude Code sessions from this machine",
              options: [
                {
                  name: "--all",
                  description: "Import every recent Claude Code conversation on this machine",
                },
                {
                  name: "--max-age",
                  description: "With --all, only import conversations updated within this many days; 0 for no limit",
                  args: {name: "days"},
                },
                {
                  name: "--limit",
                  description: "Maximum number of conversations to import with --all",
                  args: {name: "n"},
                },
                {
                  name: "--dry-run",
                  description: "Read and convert conversations without creating Amp threads",
                },
                {
                  name: "--expect-user-email",
                  description: "Require the Amp CLI to be signed in as this email",
                  args: {name: "email"},
                },
                {
                  name: "--recheck",
                  description: "With --all, ignore the local record of past imports and ask the server again (use after deleting imported threads)",
                },
                {
                  name: "--reimport",
                  description: "Import conversations again even if already imported, replacing the messages of their existing Amp threads",
                },
                {
                  name: "--json",
                  description: "Write progress as one JSON object per line (used by the Amp Mac app to run imports in the background)",
                },
                {
                  name: "--claude-dir",
                  description: "Claude Code config directory (defaults to $CLAUDE_CONFIG_DIR or ~/.claude)",
                  args: {name: "path", template: "folders"},
                },
              ],
            },
            {
              name: "opencode",
              description: "Import OpenCode sessions from this machine",
              options: [
                {
                  name: "--all",
                  description: "Import every recent OpenCode conversation on this machine",
                },
                {
                  name: "--max-age",
                  description: "With --all, only import conversations updated within this many days; 0 for no limit",
                  args: {name: "days"},
                },
                {
                  name: "--limit",
                  description: "Maximum number of conversations to import with --all",
                  args: {name: "n"},
                },
                {
                  name: "--dry-run",
                  description: "Read and convert conversations without creating Amp threads",
                },
                {
                  name: "--expect-user-email",
                  description: "Require the Amp CLI to be signed in as this email",
                  args: {name: "email"},
                },
                {
                  name: "--recheck",
                  description: "With --all, ignore the local record of past imports and ask the server again (use after deleting imported threads)",
                },
                {
                  name: "--reimport",
                  description: "Import conversations again even if already imported, replacing the messages of their existing Amp threads",
                },
                {
                  name: "--json",
                  description: "Write progress as one JSON object per line (used by the Amp Mac app to run imports in the background)",
                },
                {
                  name: "--opencode-dir",
                  description: "OpenCode data directory (defaults to $XDG_DATA_HOME/opencode or ~/.local/share/opencode)",
                  args: {name: "path", template: "folders"},
                },
              ],
            },
            {
              name: "pi",
              description: "Import Pi sessions from this machine",
              options: [
                {name: "--all", description: "Import every recent Pi conversation on this machine"},
                {
                  name: "--max-age",
                  description: "With --all, only import conversations updated within this many days; 0 for no limit",
                  args: {name: "days"},
                },
                {
                  name: "--limit",
                  description: "Maximum number of conversations to import with --all",
                  args: {name: "n"},
                },
                {
                  name: "--dry-run",
                  description: "Read and convert conversations without creating Amp threads",
                },
                {
                  name: "--expect-user-email",
                  description: "Require the Amp CLI to be signed in as this email",
                  args: {name: "email"},
                },
                {
                  name: "--recheck",
                  description: "With --all, ignore the local record of past imports and ask the server again (use after deleting imported threads)",
                },
                {
                  name: "--reimport",
                  description: "Import conversations again even if already imported, replacing the messages of their existing Amp threads",
                },
                {
                  name: "--json",
                  description: "Write progress as one JSON object per line (used by the Amp Mac app to run imports in the background)",
                },
                {
                  name: "--pi-dir",
                  description: "Pi sessions directory (defaults to $PI_CODING_AGENT_DIR/sessions or ~/.pi/agent/sessions)",
                  args: {name: "path", template: "folders"},
                },
              ],
            },
          ],
        },
        {name: "rename", description: "[alias: r] Rename a thread"},
        {
          name: "archive",
          description: "Archive a thread",
          options: [{name: "--unarchive", description: "Unarchive the thread instead of archiving"}],
        },
        {name: "delete", description: "Delete a thread"},
        {name: "markdown", description: "[alias: md] Render thread as markdown"},
        {name: "export", description: "Export a thread as JSON"},
        {name: "raw", description: "[alias: raw-thread] Export raw actor thread data as JSON"},
      ],
      options: [
        {name: "--include-archived", description: "Include archived threads in the list"},
        {
          name: "--limit",
          description: "Maximum number of threads to return",
          args: {name: "number"},
        },
        {name: "--offset", description: "Number of threads to skip", args: {name: "number"}},
      ],
    },
    {
      name: "tools",
      description: "[alias: tool] Tool management commands",
      subcommands: [
        {
          name: "list",
          description: "[alias: ls] List all active tools (including MCP tools)",
          options: [
            {name: "--json", description: "output as a single JSON array"},
            {
              name: "--mode",
              description: "filter tools by agent mode (default: medium)",
              args: {name: "mode"},
            },
          ],
        },
        {
          name: "show",
          description: "Show details about an active tool",
          options: [
            {name: "--json", description: "output as a single JSON object"},
            {
              name: "--mode",
              description: "filter tools by agent mode (default: medium)",
              args: {name: "mode"},
            },
          ],
          args: {name: "tool", description: "The tool for which to show details"},
        },
      ],
    },
    {
      name: "plugins",
      description: "[alias: plugin] Manage project, system, personal, and workspace plugins",
      subcommands: [
        {
          name: "add",
          description: "Install a plugin from a URL",
          options: [
            {
              name: "--auto-update",
              description: "Automatically update this plugin when plugins are loaded",
            },
            {
              name: "--target",
              description: "Where to put this plugin: system (default...",
              args: {name: "target"},
            },
          ],
          args: {
            name: "url",
            description: "Plugin source URL (or short form @OWNER/NAME is expanded to https://ampcode.com/@OWNER/plugins/NAME.ts)",
          },
        },
        {
          name: "remove",
          description: "[alias: rm] Remove an installed plugin",
          options: [
            {
              name: "--target",
              description: "Only remove from this location: system...",
              args: {name: "target"},
            },
          ],
          args: {
            name: "plugin",
            description: "Plugin to remove: short form @OWNER/NAME, a URL, or a plugin file name (e.g. fable-mode.ts)",
          },
        },
        {
          name: "update",
          description: "Update installed plugins or one imported shared plugin",
          options: [{name: "--force", description: "Replace committed local changes to an imported plugin"}],
          args: {
            name: "plugin",
            description: "Plugin name or path to update (defaults to all plugins with directives)",
            isOptional: true,
          },
        },
        {
          name: "list",
          description: "[alias: ls] Load and list plugins in this command, not in a running Amp session",
        },
        {
          name: "import",
          description: "Import a shared personal plugin into a plugin repository checkout",
          options: [
            {
              name: "--repository",
              description: "Personal or Workspace Plugins repository checkout",
              args: {name: "dir", template: "folders"},
            },
            {name: "--overwrite", description: "Replace a plugin with the same file name"},
          ],
          args: {name: "url", description: "Shared personal plugin URL"},
        },
        {
          name: "repositories",
          description: "[alias: repos] List available Personal Plugins and Workspace Plugins repositories",
          options: [{name: "--json", description: "Output repositories as JSON"}],
        },
        {
          name: "show-agent-options",
          description: "Show models and built-in tool names available to plugin agents",
          options: [{name: "--json", description: "Output metadata as JSON"}],
        },
        {
          name: "exec",
          description: "Execute a plugin with a given event",
          options: [{name: "--data", description: "JSON event data", args: {name: "json"}}],
          args: [
            {
              name: "plugin",
              description: "Plugin file or directory path (e.g., .amp/plugins/foo.ts)",
            },
            {name: "event", description: "Event name to send (e.g., tool.result)"},
          ],
        },
        {
          name: "show-docs",
          description: "Output the plugin API documentation understood by this version of Amp",
        },
      ],
    },
    {
      name: "skill",
      description: "[alias: skills] Manage skills from shared references, Git repositories, or local paths",
      subcommands: [
        {
          name: "add",
          description: "Install skills from a source",
          options: [
            {
              name: "--target",
              description: "Target directory for skills",
              args: {name: "dir", template: "folders"},
            },
            {
              name: "--global",
              description: "Install to global skills directory (~/.config/agents/skills/)",
            },
            {name: "--overwrite", description: "Overwrite existing skill with the same name"},
            {name: "--name", description: "Install with a custom local name", args: {name: "name"}},
          ],
          args: {name: "source", description: "Skill source (owner/repo[/path], Git URL, or local path)"},
        },
        {
          name: "list",
          description: "[alias: ls] List all available skills",
          options: [{name: "--json", description: "Output as JSON"}],
        },
        {
          name: "remove",
          description: "[alias: rm] Remove an installed skill",
          options: [
            {
              name: "--target",
              description: "Target directory for skills",
              args: {name: "dir", template: "folders"},
            },
          ],
          args: {
            name: "name",
            description: "Name of the skill to remove (skill-name or @user/skill-name)",
          },
        },
        {
          name: "info",
          description: "Show information about a skill",
          options: [{name: "--json", description: "Output as JSON"}],
          args: {name: "name", description: "Name of the skill"},
        },
        {
          name: "repositories",
          description: "[alias: repos] List available Personal Skills and Workspace Skills repositories",
          options: [{name: "--json", description: "Output repositories as JSON"}],
        },
      ],
    },
    {
      name: "permissions",
      description: "[alias: permission] Manage permissions",
      subcommands: [
        {
          name: "list",
          description: "[alias: ls] List permissions",
          options: [
            {name: "--json", description: "output as JSON"},
            {name: "--builtin", description: "output only builtin permissions"},
            {
              name: ["-w", "--workspace"],
              description: "List workspace permissions instead of global",
            },
          ],
        },
        {
          name: "test",
          description: "Test permissions",
          options: [
            {
              name: ["-c", "--context"],
              description: "Execution context (thread or subagent)",
              args: {name: "context", suggestions: ["thread", "subagent"]},
            },
            {
              name: ["-t", "--thread-id"],
              description: "Thread URL or ID for evaluation",
              args: {name: "thread"},
            },
            {name: "--json", description: "output as JSON"},
            {name: ["-q", "--quiet"], description: "suppress output, only use exit status"},
            {
              name: ["-w", "--workspace"],
              description: "Test against workspace permissions instead of global",
            },
          ],
          args: {name: "tool-name", description: "Name of the tool to test"},
        },
        {
          name: "edit",
          description: "Edit permissions",
          options: [
            {
              name: ["-w", "--workspace"],
              description: "Edit workspace permissions instead of global",
            },
          ],
        },
        {
          name: "add",
          description: "Add permission rule",
          options: [
            {
              name: "--to",
              description: "Program to delegate decision to for delegate action",
              args: {name: "program"},
            },
            {
              name: "--context",
              description: "Limit to specific context (thread or subagent)",
              args: {name: "type", suggestions: ["thread", "subagent"]},
            },
            {
              name: ["-w", "--workspace"],
              description: "Add to workspace permissions instead of global",
            },
          ],
          args: [
            {name: "action", description: "Permission action: allow, reject, ask, or delegate"},
            {name: "tool", description: "Tool name (supports globs like \"mcp__playwright__*\")"},
          ],
        },
      ],
    },
    {
      name: "mcp",
      description: "Manage MCP servers",
      subcommands: [
        {
          name: "add",
          description: "Add an MCP server configuration",
          options: [
            {
              name: "--env",
              description: "Environment variables as KEY=VALUE pairs (repeatable)",
              args: {name: "kv"},
              isRepeatable: true,
            },
            {
              name: "--header",
              description: "HTTP headers as KEY=VALUE pairs for URL-based MCP servers (repeatable)",
              args: {name: "kv"},
              isRepeatable: true,
            },
            {
              name: "--workspace",
              description: "Target workspace settings instead of global settings",
            },
          ],
        },
        {
          name: "list",
          description: "List available MCP servers",
          options: [{name: "--json", description: "Output as JSON"}],
        },
        {name: "remove", description: "Remove an MCP server configuration"},
        {
          name: "remote",
          description: "Manage MCP servers saved to ampcode.com",
          subcommands: [
            {
              name: "list",
              description: "List stored remote MCP servers",
              options: [{name: "--json", description: "Output as JSON"}],
            },
            {
              name: "add",
              description: "Add a stored remote MCP server",
              options: [
                {
                  name: "--auth",
                  description: "Authentication type: oauth, bearer, none, or workload-identity (staff access required).",
                  args: {name: "type"},
                },
                {
                  name: "--bearer-token-file",
                  description: "Read a bearer token from a file, or '-' for stdin",
                  args: {name: "path", template: "filepaths"},
                },
                {
                  name: "--headers",
                  description: "request headers as JSON [name, value] pairs",
                  args: {name: "json"},
                },
                {
                  name: "--secret-headers",
                  description: "comma-separated names of headers whose values are secret: encrypted and never shown again (default...",
                  args: {name: "names"},
                },
                {name: "--oauth-client-id", description: "OAuth client ID", args: {name: "id"}},
                {
                  name: "--oauth-client-secret-file",
                  description: "Read an OAuth client secret from a file, or '-' for stdin",
                  args: {name: "path", template: "filepaths"},
                },
                {name: "--json", description: "Output as JSON"},
              ],
            },
            {
              name: "update",
              description: "Update a stored remote MCP server",
              options: [
                {name: "--name", description: "New server name", args: {name: "name"}},
                {name: "--url", description: "New server URL", args: {name: "url"}},
                {
                  name: "--auth",
                  description: "Authentication type: oauth, bearer, none, or workload-identity (staff access required)",
                  args: {name: "type"},
                },
                {
                  name: "--bearer-token-file",
                  description: "Read a bearer token from a file, or '-' for stdin",
                  args: {name: "path", template: "filepaths"},
                },
                {
                  name: "--headers",
                  description: "request headers as JSON [name, value] pairs",
                  args: {name: "json"},
                },
                {
                  name: "--secret-headers",
                  description: "comma-separated names of headers whose values are secret: encrypted and never shown again (default...",
                  args: {name: "names"},
                },
                {name: "--oauth-client-id", description: "OAuth client ID", args: {name: "id"}},
                {
                  name: "--oauth-client-secret-file",
                  description: "Read an OAuth client secret from a file, or '-' for stdin",
                  args: {name: "path", template: "filepaths"},
                },
                {name: "--enabled", description: "Enable the server"},
                {name: "--disabled", description: "Disable the server"},
                {name: "--json", description: "Output as JSON"},
              ],
            },
            {
              name: "remove",
              description: "Remove a stored remote MCP server",
              options: [
                {name: ["-y", "--yes"], description: "Confirm removal"},
                {name: "--json", description: "Output as JSON"},
              ],
            },
            {
              name: "check",
              description: "Check a stored remote MCP server",
              options: [{name: "--json", description: "Output as JSON"}],
            },
            {
              name: "tools",
              description: "List tools for a stored remote MCP server",
              options: [
                {name: "--refresh", description: "Force a tool catalog refresh"},
                {
                  name: "--query",
                  description: "Filter tools by name or description",
                  args: {name: "query"},
                },
                {name: "--page", description: "Zero-based result page", args: {name: "page"}},
                {name: "--json", description: "Output as JSON"},
              ],
            },
            {
              name: "login",
              description: "Log in to a stored remote MCP server",
              options: [{name: "--json", description: "Output as JSON"}],
            },
            {
              name: "logout",
              description: "Log out of a stored remote MCP server",
              options: [{name: "--json", description: "Output as JSON"}],
            },
          ],
          options: [
            {
              name: "--personal",
              description: "Use remote MCP servers in your personal settings",
              isPersistent: true,
            },
            {
              name: "--workspace",
              description: "Use remote MCP servers for a workspace name or ID",
              args: {name: "workspace"},
              isPersistent: true,
            },
            {
              name: "--project",
              description: "Use remote MCP servers for a project ID or namespace/name",
              args: {name: "project"},
              isPersistent: true,
            },
            {
              name: "--current-project",
              description: "Use the current directory's Amp project",
              isPersistent: true,
            },
          ],
        },
        {
          name: "oauth",
          description: "Manage OAuth credentials for MCP servers in local CLI settings",
          subcommands: [
            {
              name: "login",
              description: "Register OAuth client credentials for an MCP server",
              options: [
                {name: "--server-url", description: "MCP server URL", args: {name: "url"}},
                {name: "--client-id", description: "OAuth client ID", args: {name: "id"}},
                {
                  name: "--client-secret",
                  description: "OAuth client secret; only necessary for clients that don't support PKCE",
                  args: {name: "secret"},
                },
                {
                  name: "--scopes",
                  description: "OAuth scopes (comma-separated)",
                  args: {name: "scopes"},
                },
                {
                  name: "--auth-url",
                  description: "OAuth authorization URL (discovered if not provided)",
                  args: {name: "url"},
                },
                {
                  name: "--token-url",
                  description: "OAuth token URL (discovered if not provided)",
                  args: {name: "url"},
                },
              ],
              args: {name: "server-name", description: "Name of the MCP server to authenticate with"},
            },
            {
              name: "logout",
              description: "Remove OAuth credentials for an MCP server",
              args: {name: "server-name", description: "Name of the MCP server"},
            },
            {
              name: "status",
              description: "Show OAuth status for an MCP server",
              args: {name: "server-name", description: "Name of the MCP server"},
            },
          ],
        },
        {name: "doctor", description: "Check MCP server status"},
        {name: "approve", description: "Approve a workspace MCP server"},
      ],
    },
    {
      name: "config",
      description: "Manage Amp configuration",
      subcommands: [
        {
          name: "edit",
          description: "Open the Amp settings file in $EDITOR",
          options: [
            {name: "--global", description: "Edit user settings (default)"},
            {name: "--workspace", description: "Edit workspace settings"},
          ],
        },
        {name: "keymap", description: "List command keymap entries"},
        {
          name: "model-providers",
          description: "Manage model provider keys and subscriptions",
          subcommands: [
            {
              name: "list",
              description: "List model provider keys and subscriptions in priority order",
              options: [
                {
                  name: "--json",
                  description: "print the complete non-secret connection data as JSON",
                },
                {
                  name: "--workspace",
                  description: "list keys of your current workspace instead of your user",
                },
              ],
            },
            {
              name: "activate",
              description: "Activate a model provider key (subscriptions other than ChatGPT replace the active connection of the same type)",
              args: {
                name: "connection-id",
                description: "ID of the model provider key to update, or an alias for your sole subscription of that type...",
                suggestions: [
                  "github_copilot",
                  "claude_sub",
                  "cursor_sub",
                  "openai_chatgpt",
                  "xai_grok",
                ],
              },
            },
            {
              name: "deactivate",
              description: "Deactivate a model provider key",
              options: [{name: ["-y", "--yes"], description: "Confirm subscription deactivation"}],
              args: {
                name: "connection-id",
                description: "ID of the model provider key to update, or an alias for your sole subscription of that type...",
                suggestions: [
                  "github_copilot",
                  "claude_sub",
                  "cursor_sub",
                  "openai_chatgpt",
                  "xai_grok",
                ],
              },
            },
            {
              name: "set-priority",
              description: "Change where a connection sits in the model routing precedence order",
              args: [
                {
                  name: "connection-id",
                  description: "ID of the model provider key to move, or an alias for your sole subscription of that type...",
                  suggestions: [
                    "github_copilot",
                    "claude_sub",
                    "cursor_sub",
                    "openai_chatgpt",
                    "xai_grok",
                  ],
                },
                {
                  name: "priority",
                  description: "new priority: 0 is tried first, higher numbers later",
                },
              ],
            },
            {
              name: "show",
              description: "Show complete non-secret model provider connection data as JSON",
              options: [
                {
                  name: "--workspace",
                  description: "read from your current workspace instead of your user",
                },
              ],
              args: {name: "id", description: "ID of the model provider key to show"},
            },
            {
              name: "delete",
              description: "Delete a model provider key or subscription",
              args: {name: "id", description: "ID of the model provider key to delete"},
            },
            {
              name: "test",
              description: "Test a model provider key or subscription",
              args: {name: "id", description: "ID of the model provider key to test"},
            },
            {
              name: "check-access",
              description: "Check which model provider serves a model by running a small inference",
              options: [
                {
                  name: "--provider-model",
                  description: "provider/model ID to check",
                  args: {name: "provider/model"},
                },
                {
                  name: "--thread",
                  description: "a thread you own to associate the inference with (defaults to AMP_THREAD_ID)",
                  args: {name: "thread-url-or-id"},
                },
                {
                  name: "--provider-key",
                  description: "specific model provider key UUID (Amp staff only)",
                  args: {name: "uuid"},
                },
                {
                  name: "--prompt",
                  description: "test prompt (Amp staff only)",
                  args: {name: "text"},
                },
                {
                  name: "--reasoning-effort",
                  description: "reasoning effort (defaults to the model routing default)",
                  args: {name: "effort"},
                },
              ],
            },
            {
              name: "add-router",
              description: "Add an AI model router and select the models routed through it",
              options: [
                {name: "--name", description: "custom connection name", args: {name: "name"}},
                {
                  name: "--model-mapping",
                  description: "newline- or comma-separated provider/model patterns served by this connection, such as */* or...",
                  args: {name: "mapping"},
                },
                {
                  name: "--api-key-file",
                  description: "read the API key from a file, or '-' for stdin",
                  args: {name: "path", template: "filepaths"},
                },
                {name: "--active", description: "activate the model router after creating it"},
                {name: "--no-active", description: "create the model router without activating it"},
                {name: "--personal", description: "add the model router to your personal account"},
                {name: "--workspace", description: "add the model router to your current workspace"},
                {
                  name: "--api-format",
                  description: "custom-url API format: chat-completions, responses, anthropic-messages, google-genai",
                  args: {name: "format"},
                },
                {
                  name: "--base-url",
                  description: "custom-url base URL, using HTTPS; optional for deepseek, zai, alibaba, kimi, fireworks, baseten, nvidia",
                  args: {name: "url"},
                },
                {
                  name: "--cloudflare-account-id",
                  description: "Cloudflare account ID",
                  args: {name: "id"},
                },
                {
                  name: "--cloudflare-gateway-id",
                  description: "Cloudflare AI Gateway ID",
                  args: {name: "id"},
                },
                {
                  name: "--workload-identity-provider",
                  description: "google-cloud-agent-platform workload identity pool provider that trusts Amp, as...",
                  args: {name: "resource-name"},
                },
                {
                  name: "--google-cloud-project",
                  description: "google-cloud-agent-platform Google Cloud project ID to bill (default: the pool's project)",
                  args: {name: "id"},
                },
                {
                  name: "--google-cloud-location",
                  description: "google-cloud-agent-platform location, a region ID or global",
                  args: {name: "location"},
                },
                {
                  name: "--iam-role-arn",
                  description: "amazon-bedrock IAM role that trusts Amp, as arn:aws:iam::ACCOUNT_ID:role/NAME",
                  args: {name: "arn"},
                },
                {
                  name: "--aws-region",
                  description: "amazon-bedrock AWS region of the Bedrock endpoint",
                  args: {name: "region"},
                },
                {
                  name: "--entra-application",
                  description: "azure-ai-foundry Entra application that trusts Amp, as TENANT_ID/CLIENT_ID",
                  args: {name: "tenant-id/client-id"},
                },
                {
                  name: "--foundry-resource",
                  description: "azure-ai-foundry Foundry resource name, the NAME in https://NAME.services.ai.azure.com",
                  args: {name: "name"},
                },
                {
                  name: "--openrouter-federation-policy-id",
                  description: "openrouter federation policy ID for workload identity federation, instead of --api-key-file",
                  args: {name: "id"},
                },
                {
                  name: "--headers",
                  description: "request headers as JSON [name, value] pairs",
                  args: {name: "json"},
                },
                {
                  name: "--secret-headers",
                  description: "comma-separated names of headers whose values are secret: encrypted and never shown again (default...",
                  args: {name: "names"},
                },
                {
                  name: "--query-params",
                  description: "custom-url query parameters as a JSON object",
                  args: {name: "json"},
                },
                {
                  name: "--router-options",
                  description: "typed request options for the selected dedicated gateway",
                  args: {name: "json"},
                },
                {
                  name: "--acknowledge-hosted-in-china",
                  description: "confirm without a prompt that you understand this provider is hosted in China",
                },
              ],
              args: {
                name: "router",
                description: "Provider to add: ollama-cloud, openrouter, vercel, cloudflare, google-cloud-agent-platform...",
              },
            },
            {
              name: "edit-router",
              description: "Edit an AI model router",
              options: [
                {name: "--name", description: "new connection name", args: {name: "name"}},
                {
                  name: "--model-mapping",
                  description: "new newline- or comma-separated provider/model patterns served by this connection; prefix a pattern...",
                  args: {name: "mapping"},
                },
                {
                  name: "--clear-model-mapping",
                  description: "reset the connection to its default models",
                },
                {
                  name: "--api-key-file",
                  description: "read a replacement API key from a file, or '-' for stdin",
                  args: {name: "path", template: "filepaths"},
                },
                {
                  name: "--api-format",
                  description: "new custom-url API format: chat-completions, responses, anthropic-messages, google-genai",
                  args: {name: "format"},
                },
                {
                  name: "--base-url",
                  description: "new custom-url base URL, using HTTPS",
                  args: {name: "url"},
                },
                {
                  name: "--cloudflare-account-id",
                  description: "Cloudflare account ID",
                  args: {name: "id"},
                },
                {
                  name: "--cloudflare-gateway-id",
                  description: "Cloudflare AI Gateway ID",
                  args: {name: "id"},
                },
                {
                  name: "--workload-identity-provider",
                  description: "new google-cloud-agent-platform workload identity pool provider",
                  args: {name: "resource-name"},
                },
                {
                  name: "--google-cloud-project",
                  description: "new google-cloud-agent-platform Google Cloud project ID; pass \"\" to bill the pool project",
                  args: {name: "id"},
                },
                {
                  name: "--google-cloud-location",
                  description: "new google-cloud-agent-platform location",
                  args: {name: "location"},
                },
                {
                  name: "--iam-role-arn",
                  description: "new amazon-bedrock IAM role ARN",
                  args: {name: "arn"},
                },
                {
                  name: "--aws-region",
                  description: "new amazon-bedrock AWS region",
                  args: {name: "region"},
                },
                {
                  name: "--entra-application",
                  description: "new azure-ai-foundry Entra application as TENANT_ID/CLIENT_ID",
                  args: {name: "tenant-id/client-id"},
                },
                {
                  name: "--foundry-resource",
                  description: "new azure-ai-foundry Foundry resource name",
                  args: {name: "name"},
                },
                {
                  name: "--openrouter-federation-policy-id",
                  description: "new openrouter federation policy ID; replaces the stored credential with workload identity federation",
                  args: {name: "id"},
                },
                {
                  name: "--headers",
                  description: "request headers as JSON [name, value] pairs",
                  args: {name: "json"},
                },
                {
                  name: "--secret-headers",
                  description: "comma-separated names of headers whose values are secret: encrypted and never shown again (default...",
                  args: {name: "names"},
                },
                {
                  name: "--query-params",
                  description: "new custom-url query parameters as a JSON object",
                  args: {name: "json"},
                },
                {
                  name: "--router-options",
                  description: "new typed request options for the dedicated gateway",
                  args: {name: "json"},
                },
                {
                  name: "--acknowledge-hosted-in-china",
                  description: "confirm without a prompt that you understand this provider is hosted in China",
                },
              ],
              args: {name: "connection-id", description: "ID of the AI model router to edit"},
            },
            {
              name: "add-key",
              description: "Add an Anthropic or OpenAI API key, or a connection that authenticates with workload identity federation instead",
              options: [
                {name: "--name", description: "custom connection name", args: {name: "name"}},
                {
                  name: "--model-mapping",
                  description: "newline- or comma-separated provider/model patterns served by this connection (the default when not...",
                  args: {name: "mapping"},
                },
                {
                  name: "--api-key-file",
                  description: "read the API key from a file, or '-' for stdin (default: ANTHROPIC_API_KEY or OPENAI_API_KEY)",
                  args: {name: "path", template: "filepaths"},
                },
                {name: "--personal", description: "add the connection to your personal account"},
                {name: "--workspace", description: "add the connection to your current workspace"},
                {
                  name: "--anthropic-organization-id",
                  description: "anthropic organization ID (00000000-0000-0000-0000-000000000000) for workload identity federation, instead of an API key",
                  args: {name: "id"},
                },
                {
                  name: "--anthropic-service-account-id",
                  description: "anthropic service account ID (svac_…) for workload identity federation, instead of an API key",
                  args: {name: "id"},
                },
                {
                  name: "--anthropic-federation-rule-id",
                  description: "anthropic federation rule ID (fdrl_…) for workload identity federation, instead of an API key",
                  args: {name: "id"},
                },
                {
                  name: "--openai-identity-provider-id",
                  description: "openai identity provider ID (idp_…) for workload identity federation, instead of an API key",
                  args: {name: "id"},
                },
                {
                  name: "--openai-service-account-id",
                  description: "openai service account ID (user-…) for workload identity federation, instead of an API key",
                  args: {name: "id"},
                },
                {
                  name: "--anthropic-workspace-id",
                  description: "anthropic workspace (wrkspc_…) the connection uses; required only for keys or federation rules...",
                  args: {name: "id"},
                },
              ],
              args: {name: "provider", description: "Provider to add: anthropic or openai"},
            },
            {
              name: "setup-guide",
              description: "Explain how to obtain the credential a provider needs; for anthropic, openai, and openrouter, the...",
              options: [
                {
                  name: "--personal",
                  description: "show the exact identity values for connections on your personal account",
                },
                {
                  name: "--workspace",
                  description: "show the exact identity values for connections on your current workspace",
                },
              ],
              args: {
                name: "provider",
                description: "Provider whose credential setup to explain: anthropic, openai, ollama-cloud, openrouter, vercel...",
              },
            },
            {
              name: "edit-key",
              description: "Edit a model provider API key: its name, model mapping, or, for anthropic and openai, its credential",
              options: [
                {name: "--name", description: "new connection name", args: {name: "name"}},
                {
                  name: "--model-mapping",
                  description: "new newline- or comma-separated provider/model patterns served by this connection; prefix a pattern...",
                  args: {name: "mapping"},
                },
                {
                  name: "--clear-model-mapping",
                  description: "reset the connection to its default models",
                },
                {
                  name: "--api-key-file",
                  description: "anthropic or openai: replace the credential with the API key in a file, or '-' for stdin",
                  args: {name: "path", template: "filepaths"},
                },
                {
                  name: "--anthropic-organization-id",
                  description: "anthropic: organization ID (00000000-0000-0000-0000-000000000000); with the other ID flags...",
                  args: {name: "id"},
                },
                {
                  name: "--anthropic-service-account-id",
                  description: "anthropic: service account ID (svac_…); with the other ID flags, switches the credential to workload identity federation",
                  args: {name: "id"},
                },
                {
                  name: "--anthropic-federation-rule-id",
                  description: "anthropic: federation rule ID (fdrl_…); with the other ID flags, switches the credential to workload identity federation",
                  args: {name: "id"},
                },
                {
                  name: "--openai-identity-provider-id",
                  description: "openai: identity provider ID (idp_…); with the other ID flags, switches the credential to workload identity federation",
                  args: {name: "id"},
                },
                {
                  name: "--openai-service-account-id",
                  description: "openai: service account ID (user-…); with the other ID flags, switches the credential to workload identity federation",
                  args: {name: "id"},
                },
                {
                  name: "--anthropic-workspace-id",
                  description: "anthropic workspace (wrkspc_…) the connection uses; required only for keys or federation rules...",
                  args: {name: "id"},
                },
                {
                  name: "--acknowledge-hosted-in-china",
                  description: "confirm without a prompt that you understand this provider is hosted in China",
                },
              ],
              args: {name: "connection-id", description: "ID of the model provider API key to edit"},
            },
            {
              name: "add-chatgpt-subscription",
              description: "Link your ChatGPT subscription to Amp as a model provider by signing in with ChatGPT in your browser",
              options: [
                {
                  name: "--legacy",
                  description: "Add a ChatGPT Subscription (Legacy) connection by signing in to OpenAI in a browser on this...",
                },
              ],
            },
            {
              name: "add-github-copilot-subscription",
              description: "Connect your personal GitHub Copilot subscription in the browser",
            },
          ],
          options: [
            {name: "--json", description: "print the complete non-secret connection data as JSON"},
            {
              name: "--workspace",
              description: "list keys of your current workspace instead of your user",
            },
          ],
        },
        {
          name: "vocabulary",
          description: "Manage personal dictation vocabulary",
          subcommands: [
            {name: "show", description: "Show personal dictation vocabulary (default)"},
            {
              name: "add",
              description: "Add terms without replacing existing vocabulary",
              args: {
                name: "terms",
                description: "Terms separated by spaces or commas; quote multi-word terms",
              },
            },
            {
              name: "set",
              description: "Replace personal dictation vocabulary; omit terms to clear it",
              args: {
                name: "terms",
                description: "Terms separated by spaces or commas; quote multi-word terms",
                isOptional: true,
              },
            },
          ],
        },
      ],
    },
    {
      name: "domains",
      description: "Manage custom domains for sites",
      subcommands: [
        {
          name: "list",
          description: "List custom domains, statuses, and required DNS records",
          options: [
            {name: "--user", description: "Use domains on your personal account"},
            {name: "--workspace", description: "Use domains on your current workspace"},
            {name: "--json", description: "Print JSON output"},
          ],
        },
        {
          name: "add",
          description: "Add a custom domain",
          options: [
            {name: "--user", description: "Use domains on your personal account"},
            {name: "--workspace", description: "Use domains on your current workspace"},
            {name: "--json", description: "Print JSON output"},
          ],
        },
        {
          name: "check",
          description: "Request an immediate DNS and provisioning status check",
          options: [
            {name: "--user", description: "Use domains on your personal account"},
            {name: "--workspace", description: "Use domains on your current workspace"},
            {name: "--json", description: "Print JSON output"},
          ],
        },
        {
          name: "remove",
          description: "[alias: rm] Remove a custom domain",
          options: [
            {name: "--user", description: "Use domains on your personal account"},
            {name: "--workspace", description: "Use domains on your current workspace"},
            {name: ["-y", "--yes"], description: "Confirm removal"},
            {name: "--json", description: "Print JSON output"},
          ],
        },
      ],
    },
    {
      name: "projects",
      description: "Manage Amp projects",
      subcommands: [
        {
          name: "create",
          description: "Create an Amp-hosted project or map an existing Git repository",
          options: [
            {name: "--orb-size", description: "Orb size", args: {name: "size"}},
            {name: "--ship-behavior", description: "Ship behavior", args: {name: "behavior"}},
            {
              name: "--custom-ship-prompt-file",
              description: "Read the Custom Ship prompt from a file, or '-' for stdin",
              args: {name: "path", template: "filepaths"},
            },
            {
              name: "--desktop-resume-script-file",
              description: "Read the Desktop resume script from a file, or '-' for stdin",
              args: {name: "path", template: "filepaths"},
            },
            {name: "--commit-author", description: "Git commit author", args: {name: "author"}},
            {name: "--amp-hosted", description: "Create a new empty repository hosted by Amp"},
            {
              name: "--icon",
              description: "Project icon image",
              args: {name: "path", template: "filepaths"},
            },
            {
              name: "--name",
              description: "Project name. Defaults to the repository name; required with --amp-hosted unless --push is used.",
              args: {name: "name"},
            },
            {name: "--personal", description: "Create the project under your personal account"},
            {
              name: "--workspace",
              description: "Create the project under your current workspace (default when set)",
            },
            {
              name: "--push",
              description: "Push the current Git repository into a new Amp-hosted repository (implies --amp-hosted; --name...",
            },
            {
              name: "--remote",
              description: "Git remote name to add for the Amp-hosted repository with --push",
              args: {name: "name"},
            },
            {
              name: "--no-git-setup",
              description: "With --push, do not configure the Git credential helper automatically (or set AMP_NO_GIT_SETUP=1)",
            },
          ],
        },
        {
          name: "update",
          description: "Update project settings by ID, [@]namespace/name, or repository",
          options: [
            {name: "--orb-size", description: "Orb size", args: {name: "size"}},
            {name: "--ship-behavior", description: "Ship behavior", args: {name: "behavior"}},
            {
              name: "--custom-ship-prompt-file",
              description: "Read the Custom Ship prompt from a file, or '-' for stdin",
              args: {name: "path", template: "filepaths"},
            },
            {
              name: "--desktop-resume-script-file",
              description: "Read the Desktop resume script from a file, or '-' for stdin",
              args: {name: "path", template: "filepaths"},
            },
            {name: "--commit-author", description: "Git commit author", args: {name: "author"}},
            {name: "--name", description: "Rename the project", args: {name: "name"}},
            {
              name: "--additional-repository",
              description: "Replace additional repositories (repeatable)",
              args: {name: "url"},
              isRepeatable: true,
            },
            {
              name: "--clear-additional-repositories",
              description: "Remove all additional repositories",
            },
            {
              name: "--base-branch",
              description: "Branch that orbs clone and Ship targets; it must exist on the remote",
              args: {name: "branch"},
            },
            {
              name: "--clear-base-branch",
              description: "Use the repository's default branch as the base branch",
            },
            {
              name: "--description",
              description: "Amp Store listing description",
              args: {name: "description"},
            },
            {
              name: "--icon",
              description: "Project icon image",
              args: {name: "path", template: "filepaths"},
            },
            {name: "--listed-on-amp-store", description: "List the project on the Amp Store"},
            {name: "--no-listed-on-amp-store", description: "Remove the project from the Amp Store"},
            {
              name: "--repository-url",
              description: "Set the primary repository URL",
              args: {name: "url"},
            },
            {
              name: "--remote-url",
              description: "Replace the Git remote URL aliases that map to this project (repeatable); the primary repository URL is always kept",
              args: {name: "url"},
              isRepeatable: true,
            },
            {name: "--clear-remote-urls", description: "Remove all Git remote URL aliases"},
            {
              name: "--repository-mode",
              description: "Link the primary repository to GitHub (github) or treat it as a plain Git URL (mapped)",
              args: {name: "mode"},
            },
            {
              name: "--pre-clone-script-file",
              description: "Read the pre-clone script from a file, or '-' for stdin",
              args: {name: "path", template: "filepaths"},
            },
            {
              name: "--pre-setup-script-file",
              description: "Read the pre-setup script from a file, or '-' for stdin",
              args: {name: "path", template: "filepaths"},
            },
          ],
        },
        {name: "list", description: "List projects accessible to you"},
        {name: "status", description: "Show the project matched by this directory's git remotes"},
        {name: "get", description: "Get a project by ID, [@]namespace/name, or repository"},
        {
          name: "snapshots",
          description: "Manage a project’s shared orb snapshots",
          subcommands: [
            {
              name: "list",
              description: "List shared orb snapshots by project and orb size",
              options: [{name: "--json", description: "Print JSON output"}],
            },
            {
              name: "delete",
              description: "[alias: rm] Delete all shared orb snapshots, or one orb size with --resource",
              options: [
                {
                  name: "--resource",
                  description: "Delete only this orb size",
                  args: {name: "environmentResource"},
                },
                {name: "--json", description: "Print JSON output"},
              ],
            },
          ],
        },
        {
          name: "gallery-media",
          description: "Manage a project’s Amp Store gallery media",
          subcommands: [
            {name: "get", description: "Print a project’s Amp Store gallery media as JSON"},
            {
              name: "set",
              description: "Set a project’s Amp Store gallery media from a JSON array of {\"type\":\"image\"|\"video\",\"url\":\"<Amp...",
            },
          ],
        },
        {
          name: "delete",
          description: "[alias: rm] Delete a project by ID, [@]namespace/name, or repository",
          options: [{name: ["-y", "--yes"], description: "Confirm deletion"}],
        },
      ],
      options: [{name: "--json", description: "Print JSON output", isPersistent: true}],
    },
    {name: "oauth-clients", description: "Manage workspace External API OAuth clients"},
    {name: "secrets", description: "Manage environment variables and secrets for orbs and boxes"},
    {
      name: "svc",
      description: "Call connected services and MCP servers through Amp",
      subcommands: [
        {
          name: "gmail",
          description: "Call the connected Gmail account",
          subcommands: [
            {
              name: "search_messages",
              description: "Search Gmail messages",
              options: [
                {
                  name: "--input",
                  description: "search_messages input as a JSON object",
                  args: {name: "json"},
                },
              ],
            },
            {
              name: "read_message",
              description: "Read one Gmail message",
              options: [
                {
                  name: "--input",
                  description: "read_message input as a JSON object",
                  args: {name: "json"},
                },
              ],
            },
            {name: "list_labels", description: "List Gmail labels"},
            {
              name: "modify_labels",
              description: "Modify Gmail message labels",
              options: [
                {
                  name: "--input",
                  description: "modify_labels input as a JSON object",
                  args: {name: "json"},
                },
              ],
            },
          ],
        },
      ],
      options: [{name: "--input", description: "tool input as a JSON object", args: {name: "json"}}],
      args: [
        {name: "service", description: "MCP server name"},
        {name: "tool", description: "MCP tool name"},
      ],
    },
    {
      name: "usage",
      description: "Show your current Amp usage and credit balance",
      options: [
        {name: "--details", description: "Show detailed credit, token, and thread usage"},
        {
          name: "--start",
          description: "Start of the usage range as an ISO 8601 timestamp",
          args: {name: "timestamp"},
        },
        {
          name: "--end",
          description: "End of the usage range as an ISO 8601 timestamp",
          args: {name: "timestamp"},
        },
      ],
    },
    {
      name: "update",
      description: "[alias: up] Update Amp CLI",
      options: [
        {
          name: "--target-version",
          description: "Update to a specific version",
          args: {name: "version"},
        },
        {
          name: "--porcelain",
          description: "Print a machine-readable result to stdout: \"updated <version>\" or \"no update needed\"",
        },
      ],
    },
    {
      name: "completions",
      description: "Print a shell completion script",
      args: {name: "shell", description: "Shell to configure"},
    },
  ],
  options: [
    {
      name: "--visibility",
      description: "Set thread visibility (private, unlisted, workspace, group)",
      args: {name: "visibility", suggestions: ["private", "unlisted", "workspace", "group"]},
    },
    {name: ["-V", "--version"], description: "Print the version number and exit"},
    {name: "-v", description: "Alias for --version"},
    {
      name: "--notifications",
      description: "Enable notification alerts (audio locally, terminal bell over SSH or with AMP_FORCE_BEL; enabled by...",
    },
    {
      name: "--no-notifications",
      description: "Disable notification alerts (audio locally, terminal bell over SSH or with AMP_FORCE_BEL; enabled...",
    },
    {
      name: "--color",
      description: "Enable color output (enabled by default if stdout and stderr are sent to a TTY)",
    },
    {
      name: "--no-color",
      description: "Disable color output (enabled by default if stdout and stderr are sent to a TTY)",
    },
    {
      name: "--settings-file",
      description: "Custom settings file path (overrides the default location...",
      args: {name: "value"},
    },
    {
      name: "--log-level",
      description: "Set log level (parent, children, category, sinks, parentSinks, filters, lowestLevel, contextLocalStorage)",
      args: {
        name: "value",
        suggestions: [
          "parent",
          "children",
          "category",
          "sinks",
          "parentSinks",
          "filters",
          "lowestLevel",
          "contextLocalStorage",
        ],
      },
    },
    {
      name: "--log-file",
      description: "Set log file location (overrides the default location...",
      args: {name: "value"},
    },
    {name: "--ide", description: "Enable IDE connection (default)."},
    {name: "--no-ide", description: "Disable IDE connection"},
    {
      name: "--mcp-config",
      description: "JSON configuration or file path for MCP servers to merge with existing settings",
      args: {name: "value"},
    },
    {
      name: ["-m", "--mode"],
      description: "Set the agent mode (low, medium, high, ultra, claude-code, or a plugin mode by key or label...",
      args: {name: "value"},
    },
    {
      name: "--features",
      description: "Enable comma-separated thread features (known: fast, Faster serving for supported models, billed at...",
      args: {name: "value"},
    },
    {
      name: "--fast",
      description: "Run new threads in Fast mode (faster serving for supported models, billed at a premium); alias for --features fast",
    },
    {
      name: "--executor",
      description: "Run a new thread locally, in an orb, or on a runner (local, orb, runner:<id>)",
      args: {name: "value"},
    },
    {
      name: "--runner-dir",
      description: "With --executor runner:<id>, select one absolute directory served by the runner for the new thread.",
      args: {name: "value", template: "folders"},
    },
    {
      name: "--no-tui",
      description: "Run Amp without a TUI: serve remote thread creation requests for the current directory.",
    },
    {
      name: "--share",
      description: "Share this --no-tui runner with your workspace while it is running.",
    },
    {
      name: "--desktop",
      description: "Share a desktop with threads served by this --no-tui runner: this Mac’s display, or a private virtual desktop on Linux.",
    },
    {
      name: "--share-network",
      description: "Let your orbs browse the web through this --no-tui runner’s network connection.",
    },
    {
      name: "--amp-env",
      description: "Inject the Secrets & Env Vars configured on ampcode.com (workspace, project, and personal) into the...",
    },
    {
      name: "--remote-control-terminal",
      description: "Enable terminal access from ampcode.com for threads accessed across clients",
    },
    {
      name: "--no-remote-control-terminal",
      description: "Disable terminal access from ampcode.com for threads accessed across clients",
    },
    {
      name: ["-x", "--execute"],
      description: "Use execute mode, optionally with user message.",
      args: {name: "message", isOptional: true},
    },
    {
      name: ["-ox", "--orb-execute"],
      description: "When used with -x/--execute, execute in an async orb on the Amp server.",
    },
    {
      name: "--attach",
      description: "With remote execute mode (such as -ox), upload a local file and add its URL to the message.",
      args: {name: "path", template: "filepaths"},
    },
    {
      name: "--project",
      description: "With the orb executor, run the new thread on this Amp project (namespace/name, owner/repo, or...",
      args: {name: "project", isOptional: true},
    },
    {
      name: "--orb-size",
      description: "With the orb executor, use this size instead of the project default: one of a1.tiny, a1.small...",
      args: {name: "size"},
    },
    {
      name: "--orb-provider",
      description: "With --orb-execute, run the new orb on the workspace or personal orb provider with this name (as...",
      args: {name: "provider"},
    },
    {
      name: "--title",
      description: "With --execute, set the title of the new thread before starting the agent.",
      args: {name: "title"},
    },
    {
      name: "--stream-json",
      description: "When used with --execute, output in Claude Code-compatible stream JSON format instead of plain text.",
    },
    {
      name: "--stream-json-thinking",
      description: "Include thinking blocks in stream JSON output (non-Claude Code extension). Implies --stream-json.",
    },
    {
      name: "--stream-json-input",
      description: "Read JSON Lines user messages from stdin. Requires both --execute and --stream-json.",
    },
    {
      name: "--plugin-ready-timeout",
      description: "When used with --execute, how long to wait for plugins to become ready before running the turn, so...",
      args: {name: "seconds", isOptional: true},
    },
    {
      name: "--timings",
      description: "With --execute, print startup and execution timings to stderr.",
    },
    {
      name: "--no-archive-after-execute",
      description: "When used with --execute on a new thread or with the review command, leave the thread unarchived...",
    },
    {
      name: ["-l", "--label"],
      description: "Add a label to the thread created or continued by this command. Repeat the flag for multiple labels.",
      args: {name: "label"},
      isRepeatable: true,
    },
  ],
};
