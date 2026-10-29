### What's new in this release
- Based on Zed v1.26.2.
- The upstream sync workflow puts merge instructions in the run summary when issues are disabled on the repository, instead of failing on the issue step.

### Features
- Edit previous messages for agents that advertise `claudeCodePlus` truncate support (claude-agent-acp-plus 0.94.0-plus.6+).
- `/clear`, `/reset` and `/new` open a new thread for the same agent and carry the thread title over, when the agent advertises `newThreadCommands` (claude-agent-acp-plus 0.94.0-plus.8+).
- Plan usage limits (session and weekly) in the context usage tooltip as compact rows, and a usage ring in the composer that includes the weekly limit (claude-agent-acp-plus 0.94.0-plus.10+).
- Prompt cache status indicator in the composer, matching the VS Code extension.
- **Run in background** link under running Bash, PowerShell and Agent tool calls that the agent marks as foreground.
- Background task counter in the composer with a list to inspect, stop and find tasks (claude-agent-acp-plus 0.94.0-plus.20+).
- Per-thread **Expand Thinking Blocks** toggle in the agent panel menu, overriding `thinking_display` for that thread.
- Markers on the thread scrollbar for your prompts, Claude's messages and its thinking, with a preview on hover.
- `agent.wrap_terminal_commands` setting and a hover button on command blocks to toggle wrapping.
- `agent.show_add_context_button`, `agent.show_follow_button` and `agent.hidden_config_options` settings to hide message editor controls.
- Thread history shows the last interaction time.
- Tool call locations without a text buffer, such as images, can be opened.
- Agent login runs the agent's own command and shows login errors.
- Dock panel sizes are shared across projects.
- Project panel entries can be pasted into other projects.
- Auto-updates from this repository's releases, and a one-click installer script.
