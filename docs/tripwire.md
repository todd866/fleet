# tripwire

Prevention on these Macs is deliberately light: agents run without permission prompts and
unattended jobs must restart by themselves. `tripwire` is the other half. It watches the few
things that have to change for someone to get in, stay in, or switch the guards off, and when
one changes it starts an agent to find out why.

| Wire | Raises when |
|---|---|
| `files` | a login, shell-startup, sudo, ssh or guard-config file changes (`files` in the config) |
| `guards` | a provider's hook file stops calling the guard (`must_contain`) |
| `launchd` | a plist appears whose label the job registry does not list, or any plist changes |
| `listeners` | a program starts listening beyond loopback that never has before |
| `accounts` | a local user appears, or someone joins the admin group |
| `public_repos` | a repository of the signed-in GitHub account becomes public (`"github": true` in the config; one Mac is enough) |

The first `tripwire check` records a baseline and raises nothing. After that a change raises
once, and the baseline moves on. An incident is a folder under
`~/.local/state/fleet/tripwire/incidents/` holding `incident.json` (the evidence), `brief.md`
(what the agent was told), `agent.log` and the agent's `report.md`, whose first line is
`VERDICT: BENIGN`, `SUSPICIOUS` or `HOSTILE`. The owner gets a notification and, where `mail_to`
is set, agent mail when the incident is raised, and again if the verdict is not benign.

## Hold first, then think

When an agent is about to look, the changed thing is paused at once, in a way that can be
undone, so that taking a few minutes to think costs nothing:

| Change | Hold | Release |
|---|---|---|
| new listener | its process is stopped (`SIGSTOP`) | continued (`SIGCONT`), if it is still the same process |
| new or changed job in `~/Library/LaunchAgents` | disabled, and unloaded if it was loaded | enabled, and loaded again if it had been |
| login keys, a guard hook file (`hold_files`, `must_contain`) | put back to the last checked version; the changed version is kept in the incident's `held/` | the changed version returns, unless the file changed again meanwhile |

Other watched files, system-wide jobs and accounts are investigated but not held: holding them
needs root or would undo ordinary edits. The job that runs `tripwire` is never held, nor are
the programs in `never_pause`.

The bundled investigator returns its report on stdout, using Gemini's sandboxed plan mode or
Z.ai with only read, glob and grep tools. The wrapper saves the first complete verdict atomically.
On its next check Tripwire releases a hold whose report begins with the exact line `VERDICT: BENIGN`.
Any
other verdict leaves the hold in place and tells the owner, who releases it with
`tripwire release <incident> --owner`. When the agent never reports, after
`hold_timeout_min` (default 60) a stopped process or job goes on again, because that hold
costs work; login keys and guard hook files stay put back until the owner releases them,
because that hold costs nothing. The owner is told either way. An agent that exits
without a report (out of allowance, crashed) is noticed at the next check: `summon_fallback`,
if set, is started once with the same brief (a second model, say), and failing that the same
release happens at once rather than after the hour. Nothing is held when no agent can be
started (the daily limit, no agent command): a pause with nobody coming is just an outage.
At most `max_summons_per_day` agents (default 4) start on one Mac in a day; past that the
incident is still recorded and announced. `"hold": false` turns holding off.

The cost is honest: the first time a program listens on the network, or a job is installed
outside the registry, it stops for the few minutes the agent takes. Listeners are remembered
once seen, and a job listed in the registry before it is installed is not raised at all.

`tripwire status` lists wires, incidents and holds. `tripwire accept` takes the current state
as the baseline after a change you made yourself. `tripwire test` raises a harmless incident
(`--summon` also starts the agent). Install: link `bin/tripwire` into `~/.local/bin` and load
[launchd/com.example.tripwire.plist](launchd/com.example.tripwire.plist). Config:
`~/.config/fleet/tripwire.json`, every key optional (see the top of `bin/tripwire`).

What it does not see: anything that changes nothing on this list (a secret read and sent out,
a process that never listens), a change made and reverted between two checks, and an intruder
who edits the baseline. It is a tripwire, not a monitor.
