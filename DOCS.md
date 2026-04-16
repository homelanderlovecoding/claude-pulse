# Pulse – Physical Button Bridge for Claude Code

Pulse is a host daemon that bridges Claude Code with a physical button (or web simulator). It watches a state file for changes made by Claude Code hooks and broadcasts them over WebSocket to connected button clients. Button presses are received over the same WebSocket and logged for Claude Code to pick up.

## Architecture

```
Claude Code hooks  -->  ~/.pulse/state.json  -->  pulse-daemon  --WS-->  Button (web / ESP32)
                                                       |
Button press  <--  ~/.pulse/actions.log  <--  pulse-daemon  <--WS--  Button
```

## Quick Start

### 1. Build the daemon

```bash
cd daemon
cargo build --release
```

The binary will be at `daemon/target/release/pulse-daemon`.

### 2. Run the daemon

```bash
# With defaults (port 3456, state file ~/.pulse/state.json)
./daemon/target/release/pulse-daemon

# Custom port and state file
./daemon/target/release/pulse-daemon --port 4000 --state-file /tmp/pulse-state.json
```

### 3. Configure Claude Code hooks

Add the hook scripts to your Claude Code configuration so they fire at the right lifecycle events:

| Event         | Script                    |
|---------------|---------------------------|
| Task start    | `hooks/on_task_start.sh`  |
| Task done     | `hooks/on_task_done.sh`   |
| Prompt needed | `hooks/on_prompt.sh`      |
| Error         | `hooks/on_error.sh`       |

Each script writes a JSON state to `~/.pulse/state.json`, which the daemon picks up and broadcasts.

### 4. Open the web simulator

Open the Pulse web simulator in a browser. It connects to `ws://localhost:3456` and shows the current state with action buttons.

### 5. Test manually

```bash
# In one terminal, start the daemon:
cargo run --manifest-path daemon/Cargo.toml

# In another terminal, write a state change:
echo '{"state":"needs_input","message":"Approve this?"}' > ~/.pulse/state.json

# In another terminal, connect with websocat (or any WS client):
websocat ws://127.0.0.1:3456

# Send a button action:
{"action":"approve","detail":"looks good"}
```

## Files

| Path                     | Purpose                                        |
|--------------------------|-------------------------------------------------|
| `~/.pulse/state.json`   | Current agent state (written by hooks)          |
| `~/.pulse/actions.log`  | Log of all button actions with timestamps       |
| `~/.pulse/learn.json`   | Accumulated reject_learn entries for training    |

## WebSocket Protocol

**Server to client** (state updates):
```json
{"state": "working", "message": "Agent started"}
```

**Client to server** (button actions):
```json
{"action": "approve", "detail": "optional context"}
```

Supported actions: `approve`, `reject_learn`, `security_scan`, `explain`.
