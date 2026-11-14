# Switching provider

A Cursor-only day works when `agentkit doctor --provider cursor` exits 0. That
proves the generated rules match the site, the five guards are named in
`~/.cursor/hooks.json`, `agent-context` is on session start and before each
prompt, the notebook is readable, and the guard self-test still denies a
hard delete and an iCloud-wide trash. It does not start Cursor.

## Checklist

1. Copy `agentkit/example/` to `~/.config/agentkit`. Do not commit that copy.
2. Put `agentkit` and `agent-context` on `PATH` (`~/.local/bin`). Generated hooks call those names. Doctor checks the hook file and the decision logic, not which file answered.
3. Put the real global rules in `rules/global.md` and each project in `rules/projects/<name>.md`. The generated global file also lists the five prohibitions, so a surface that skips its hook file still has them as text. Text is not enforcement.
4. Fill `guards.air_aliases` and `guards.allowed_cd` from the live hook before the first apply. An empty air list denies worker deletes. That is stricter, and it is easy to miss.
5. If addresses must not appear in commands, set `guards.email_substrings_file` to a file that stays out of git.
6. `agentkit sync` and read the diff.
7. `agentkit sync --apply`.
8. `agentkit memory import --from ~/.claude/projects`. Read the notebook before trusting it. Secret-shaped files are skipped, not reviewed.
9. `agentkit doctor --provider cursor`.
10. Open a Cursor session. Confirm it loaded `AGENTS.md` and `~/.cursor/rules/agentkit-global.mdc`. Run `agent-context`. Send yourself mail and confirm the next prompt shows `N unread`.
11. Try a command that must be denied (`rm` of a project file, `trash ~/Documents`, `git push --force`). The hook row is `beforeShellExecution` with `failClosed: true` on the destructive guards.

`agentkit doctor` with no provider is the report fleet-health and mac-registry consume (`~/.local/state/agentkit/doctor.json`).

## What each provider actually enforces

| | Claude Code | Codex | Cursor |
|---|---|---|---|
| Guards | `PreToolUse` on `Bash` in `~/.claude/settings.json` | `PreToolUse` in `~/.codex/hooks.json` (matcher `.*`; the guard ignores non-shell tools) | `beforeShellExecution` in `~/.cursor/hooks.json` |
| Context | `SessionStart` and `UserPromptSubmit` run `agent-context` | same events | `sessionStart` and `beforeSubmitPrompt` |
| Rules | `~/.claude/CLAUDE.md` and project `CLAUDE.md` (`@AGENTS.md`) | `~/.codex/AGENTS.md` and project `AGENTS.md` | `~/.cursor/rules/agentkit-global.mdc`, skill rules, project `AGENTS.md` |
| Skills | `~/.claude/skills/<name>/` | `~/.codex/skills/<name>/` | a rule plus `~/.cursor/commands/<name>.md` |

Codex already runs pre-tool hooks. No shell wrapper is required while `hooks.json` is loaded. If a Codex build ignores that file, the nearest control is `approval_policy` in `config.toml`, which is not these guards. The generated `AGENTS.md` still states the rules.

## Gaps

- **Cursor has no status line.** Capacity and mail show up when `agent-context` runs, and the always-applied rule tells the session to read it. Between prompts there is no footer.
- **Cursor may not load `~/.cursor/hooks.json` on every surface.** Doctor checks the file and the self-test. It cannot see a running IDE. If a surface skips the file, the rules are instructions, not enforcement. Treat that day as degraded.
- **Neither Cursor nor Codex has `ScheduleWakeup` or a shared background-task handle.** Overnight work is a shell loop or launchd. A session that ends will not wake itself.
- **Codex has hooks and no status line.** Same mitigation as Cursor: `agent-context` on session start and each prompt.
- **Any provider:** an agent that can edit its hook file can turn a guard off. That was already true of Claude's `settings.json`. Do not leave the hook file writable on an unattended day. Doctor flags a missing command.
- **MCP tokens are not in the site.** Sync does not read or write them.
- **The worker hard-delete exemption is config.** Set it before apply or deletes on that machine stay denied.
- **Email-in-command** stays on the legacy Claude hook until `guards.email_substrings_file` exists. Sync adds agentkit hooks; it does not remove the legacy ones.
- **Alternate launcher notebooks** under a third-party config directory are a second store. This crate does not merge them.
- **Project-only guards** (shared-tree `git add -A`, worktree `db:seed`, direct database writes) are not generated. Keep that project's hook.

## Rollout

1. Land this crate. Do not point a live home at it yet.
2. Fill the private site from the current rules. Dry-run `sync` and keep the diff.
3. Apply on one machine. `agentkit doctor` must exit 0.
4. Import memory. Read the index.
5. Run `agentkit doctor --provider cursor` and the checklist above.
6. Let fleet-health and mac-registry read `doctor.json`. Until a machine has a report, fleet-health lists `agentkit: no doctor report`.
7. Leave the legacy Claude hooks in place until a week of doctor reports stay clean. Then remove only the hooks whose command this generator replaced. Do not remove a project hook this crate does not generate.
