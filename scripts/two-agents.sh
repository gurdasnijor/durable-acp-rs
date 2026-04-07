#!/bin/bash
# Launch two durable-acp agents in separate terminal tabs.
# Each tab runs: chat -> conductor -> claude-agent-acp
#
# Usage: ./scripts/two-agents.sh

set -e
cd "$(dirname "$0")/.."

cargo build --release 2>/dev/null

BIN="$(pwd)/target/release"

echo "Opening two terminal tabs..."
echo "  agent-a: port 4437 (API: 4438)"
echo "  agent-b: port 4439 (API: 4440)"
echo ""
echo "Once both sessions start, ask agent-a:"
echo "  'Use list_agents to find peers, then prompt_agent to ask agent-b to write a haiku'"
echo ""

# macOS: open new Terminal tabs
if command -v osascript &>/dev/null; then
    osascript -e "
        tell application \"Terminal\"
            activate
            do script \"cd $(pwd) && $BIN/chat $BIN/durable-acp-rs --name agent-a --port 4437 npx @agentclientprotocol/claude-agent-acp\"
            do script \"cd $(pwd) && $BIN/chat $BIN/durable-acp-rs --name agent-b --port 4439 npx @agentclientprotocol/claude-agent-acp\"
        end tell
    "
else
    echo "Not on macOS. Run these in two separate terminals:"
    echo ""
    echo "  Terminal 1: $BIN/chat $BIN/durable-acp-rs --name agent-a --port 4437 npx @agentclientprotocol/claude-agent-acp"
    echo "  Terminal 2: $BIN/chat $BIN/durable-acp-rs --name agent-b --port 4439 npx @agentclientprotocol/claude-agent-acp"
fi
