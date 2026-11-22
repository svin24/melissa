#!/usr/bin/env python3
"""A scripted ACP agent for the UI screenshot harness (dev only, never shipped).

Speaks just enough of ACP v1 (JSON-RPC over stdin/stdout) for Melissa to open sessions and
run turns, and answers every prompt from a fixed script, so the chats in the screenshots are
the same on every run and go through Melissa's real code paths (step cards, a permission
card, a running turn, models for the picker). It never touches the network or the disk.

The script is picked by a keyword in the user's message:
  "disk"   a thought, two finished steps and a Markdown reply
  "clean"  a step that asks for permission (Melissa shows the permission card) and waits;
           the turn keeps running until the request is answered or the turn is cancelled
  other    a one-paragraph reply
"""

import json
import sys
import threading

MODELS = {
    "availableModels": [
        {"modelId": "shots-large", "name": "Shots Large"},
        {"modelId": "shots-small", "name": "Shots Small"},
    ],
    "currentModelId": "shots-large",
}

DISK_REPLY = """The disk has room. Here is what uses the most space:

| Folder | Size |
| --- | --- |
| `/var/lib/containers` | 18 GB |
| `/home` | 9.4 GB |
| `/var/log` | 1.2 GB |

1. Old container images take most of it.
2. Logs are rotated weekly.

Run `podman image prune` to free about **6 GB**, or ask me to do it. Details: [podman docs](https://example.com/podman)."""

SHORT_REPLY = "Sure. I keep an eye on the apps and the disk, and tell you when something needs you."

_out = threading.Lock()
_pending = {}  # our request id -> (Event, [reply])
_cancelled = threading.Event()
_next_id = [0]


def send(msg):
    with _out:
        sys.stdout.write(json.dumps(msg) + "\n")
        sys.stdout.flush()


def update(session, body):
    send({"jsonrpc": "2.0", "method": "session/update",
          "params": {"sessionId": session, "update": body}})


def chunk(session, kind, text):
    update(session, {"sessionUpdate": kind, "content": {"type": "text", "text": text}})


def request(method, params):
    with _out:
        _next_id[0] += 1
        rid = f"fake-{_next_id[0]}"
    done = threading.Event()
    slot = []
    _pending[rid] = (done, slot)
    send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
    while not done.wait(0.2):
        if _cancelled.is_set():
            return None
    return slot[0] if slot else None


def user_message(params):
    text = "\n".join(b.get("text", "") for b in params.get("prompt", []) if b.get("type") == "text")
    marker = "The message:\n\n"
    if marker in text:
        text = text.rsplit(marker, 1)[1]
    return text.strip().lower()


def run_turn(rid, params):
    session = params.get("sessionId", "")
    message = user_message(params)
    stop = "end_turn"
    if "disk" in message:
        chunk(session, "agent_thought_chunk", "The owner asks about disk space; check the usage first.")
        update(session, {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read /etc/fstab",
                         "kind": "read", "status": "pending", "rawInput": {"path": "/etc/fstab"}})
        update(session, {"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"})
        update(session, {"sessionUpdate": "tool_call", "toolCallId": "t2", "title": "melissa__device_info",
                         "kind": "other", "status": "pending", "rawInput": {}})
        update(session, {"sessionUpdate": "tool_call_update", "toolCallId": "t2", "status": "completed"})
        chunk(session, "agent_message_chunk", DISK_REPLY)
    elif "clean" in message:
        chunk(session, "agent_message_chunk", "I can remove the old images. That needs your OK.")
        call = {"toolCallId": "t3", "title": "Run podman image prune --all", "kind": "execute",
                "status": "pending", "rawInput": {"command": "podman image prune --all"}}
        update(session, dict(call, sessionUpdate="tool_call"))
        answer = request("session/request_permission", {
            "sessionId": session,
            "toolCall": call,
            "options": [
                {"optionId": "allow-once", "name": "Allow", "kind": "allow_once"},
                {"optionId": "reject-once", "name": "Reject", "kind": "reject_once"},
            ],
        })
        if answer is None or _cancelled.is_set():
            stop = "cancelled"
        else:
            update(session, {"sessionUpdate": "tool_call_update", "toolCallId": "t3", "status": "completed"})
            chunk(session, "agent_message_chunk", "Removed the old images.")
    else:
        chunk(session, "agent_message_chunk", SHORT_REPLY)
    _cancelled.clear()
    send({"jsonrpc": "2.0", "id": rid, "result": {"stopReason": stop}})


def main():
    sessions = 0
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        method = msg.get("method")
        rid = msg.get("id")
        params = msg.get("params") or {}
        if method is None:
            # A reply to one of our requests (the permission answer).
            entry = _pending.pop(rid, None)
            if entry:
                entry[1].append(msg.get("result"))
                entry[0].set()
            continue
        if method == "initialize":
            result = {
                "protocolVersion": 1,
                "agentCapabilities": {
                    "loadSession": False,
                    "mcpCapabilities": {"http": True, "sse": False},
                    "promptCapabilities": {"image": False},
                },
                "agentInfo": {"name": "ui-shots-agent", "version": "1.0.0"},
                "authMethods": [],
            }
        elif method == "session/new":
            sessions += 1
            result = {"sessionId": f"shots-{sessions}", "models": MODELS}
        elif method == "session/prompt":
            threading.Thread(target=run_turn, args=(rid, params), daemon=True).start()
            continue
        elif method == "session/cancel":
            _cancelled.set()
            continue
        else:
            result = {}
        if rid is not None:
            send({"jsonrpc": "2.0", "id": rid, "result": result})


if __name__ == "__main__":
    main()
