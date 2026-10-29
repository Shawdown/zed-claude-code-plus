### What's new in this release
- The thread scrollbar also marks Claude's messages to you (a short bar on the right half of the track) and its thinking (a small dot at the right edge), so they can be told apart from your prompts. Hovering a marker shows "Claude" or "Thinking" with the first line of the text, and clicking it scrolls there.
- Pairs with claude-agent-acp-plus 0.94.0-plus.18, which shows usage limit and other errors from turns the agent starts on its own as a single red error box, and reports the correct context usage after a thread is reopened.

### Features
- Edit previous messages for agents that advertise `claudeCodePlus` truncate support (claude-agent-acp-plus 0.94.0-plus.6+).
- `/clear`, `/reset` and `/new` open a new thread for the same agent and carry the thread title over, when the agent advertises `newThreadCommands` (claude-agent-acp-plus 0.94.0-plus.8+).
- Plan usage limits (session and weekly) in the context usage tooltip as compact rows, and a usage ring in the composer that includes the weekly limit (claude-agent-acp-plus 0.94.0-plus.10+).
- Prompt cache status indicator in the composer, matching the VS Code extension.
- **Run in background** link under running Bash, PowerShell and Agent tool calls that the agent marks as foreground.
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
