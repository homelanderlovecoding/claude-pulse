#!/bin/bash
# Pulse MCP Server Installer
# Outputs the Claude Code settings.json config for these MCP servers

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cat <<EOF
Add the following to your Claude Code settings.json (typically at ~/.claude/settings.json):

{
  "mcpServers": {
    "pulse-memory": {
      "command": "node",
      "args": ["${SCRIPT_DIR}/memory-server.js"]
    },
    "pulse-learning": {
      "command": "node",
      "args": ["${SCRIPT_DIR}/learning-server.js"]
    },
    "pulse-security": {
      "command": "node",
      "args": ["${SCRIPT_DIR}/security-server.js"]
    }
  }
}

To install automatically, run:
  claude mcp add pulse-memory node "${SCRIPT_DIR}/memory-server.js"
  claude mcp add pulse-learning node "${SCRIPT_DIR}/learning-server.js"
  claude mcp add pulse-security node "${SCRIPT_DIR}/security-server.js"
EOF
