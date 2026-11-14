"""Provider hook snippets, and the one presence event every provider reduces to.

Snippets are printed for a person to install. Nothing here edits a settings file.
"""
import hashlib
import json
import uuid
from pathlib import Path

from cowork.model import EVENT_MAP, canonical_provider, session_key

PROVIDERS = ("claude", "codex", "cursor", "agentkit", "local", "cloud")


def _command(provider, event):
    return f"cowork hook {provider} {event}"


def _commands_for(provider, event):
    if event in ("PreToolUse", "preToolUse"):
        return [f"cowork observe --provider {provider}"]
    commands = [_command(provider, event)]
    if event in ("UserPromptSubmit", "beforeSubmitPrompt"):
        commands.append("cowork context")
    return commands


def _claude_shaped(provider, events):
    hooks = {}
    for event in events:
        hooks[event] = [{
            "hooks": [{
                "type": "command",
                "command": command,
                "timeout": 2,
            } for command in _commands_for(provider, event)]
        }]
    return {"hooks": hooks}


def _cursor_shaped():
    events = ("sessionStart", "beforeSubmitPrompt", "preToolUse", "postToolUse", "stop", "sessionEnd")
    hooks = {
        event: [{"command": command, "timeout": 2} for command in _commands_for("cursor", event)]
        for event in events
    }
    child = (
        'cowork child "$COWORK_WORK" --work "$COWORK_CHILD" --provider cursor '
        '--session "$COWORK_SESSION" --host "$COWORK_HOST" '
        '--scope "worktree:${COWORK_WORKTREE}" --ttl "${COWORK_TTL:-3600}"'
    )
    return {
        "version": 1,
        "hooks": hooks,
        "worker": {"cursor-worker": child, "cursor-remote": child},
    }


def _shell(provider):
    return "\n".join([
        f'cowork join --provider {provider} --session "$COWORK_SESSION" '
        f'--host "$COWORK_HOST" --scope "repo:${{COWORK_REPO:-work}}" '
        f'--work "${{COWORK_WORK:-session}}"',
        f'cowork heartbeat "${{COWORK_WORK:-session}}" --session "$COWORK_SESSION" '
        f'--provider {provider} --host "$COWORK_HOST"',
        f'cowork leave --session "$COWORK_SESSION" --provider {provider} --host "$COWORK_HOST"',
        "",
    ])


def snippet(provider):
    name = canonical_provider(provider)
    if name == "claude":
        return json.dumps(_claude_shaped("claude", (
            "SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop", "SessionEnd",
        )), indent=2) + "\n"
    if name == "codex":
        return json.dumps(_claude_shaped("codex", (
            "SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop", "SessionEnd", "Interrupt",
        )), indent=2) + "\n"
    if name == "cursor":
        return json.dumps(_cursor_shaped(), indent=2) + "\n"
    if name == "agentkit":
        body = {
            "provider": "agentkit",
            "context_hook": {
                "session.start": _command("agentkit", "SessionStart"),
                "prompt": _command("agentkit", "UserPromptSubmit"),
                "session.end": _command("agentkit", "SessionEnd"),
                "context": "cowork context",
                "observe": "cowork observe --provider agentkit",
            },
        }
        return json.dumps(body, indent=2) + "\n"
    if name == "local":
        return _shell("local")
    if name == "cloud":
        return _shell("cloud")
    raise ValueError(name)


def clean_event(raw, provider, host, event_name, now):
    """Reduce a provider payload to a presence event. Drops prompt and tool text."""
    if not isinstance(raw, dict) or event_name not in EVENT_MAP:
        return None
    sid = raw.get("session_id") or raw.get("conversation_id")
    if not isinstance(sid, str) or not sid or len(sid) > 512:
        return None
    provider = canonical_provider(provider)
    cwd = raw.get("cwd") if isinstance(raw.get("cwd"), str) else ""
    project = Path(cwd).name if cwd else "unmapped"
    if not project or project in (".", ".."):
        project = "unmapped"
    label = project if project != "unmapped" else ""
    event = EVENT_MAP[event_name]
    if raw.get("status") == "error":
        event = "error"
    elif raw.get("status") == "aborted":
        event = "interrupted"
    key = session_key(provider, host, sid)
    native = raw.get("tool_use_id")
    if isinstance(native, str) and native:
        event_id = hashlib.sha256(f"{key}|{event}|{native}".encode()).hexdigest()
    else:
        event_id = uuid.uuid4().hex
    return {
        "id": key,
        "event_id": event_id,
        "engine": provider,
        "host": host,
        "project": project,
        "label": label,
        "event": event,
        "at": int(now),
        "source": "local lifecycle hook",
    }
