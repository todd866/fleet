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
(what the agent was told), `agent.log` and the agent's `report.md`, whose first line is exactly
`VERDICT: BENIGN`, `VERDICT: NEEDS REVIEW`, `VERDICT: SUSPICIOUS` or `VERDICT: HOSTILE`,
followed by an explanation. The owner gets a notification and, where `mail_to`
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

Gemini runs in sandboxed plan mode. The Z.ai route sends a bounded set of typed
observations to the installed loopback proxy. To use an already configured DeepSeek
account after Z.ai fails, set the fallback command to
`["tripwire-investigate", "--engine", "zai", "--last-resort", "deepseek"]`.
Both text-only routes omit file contents, paths, process arguments and tool definitions.
Their metadata can establish a benign cause for the explicit local CLI drill. A benign
guess about an ordinary incident becomes NEEDS REVIEW. The investigator records provider
outcomes in the incident directory.

Tripwire saves complete verdicts atomically. On its next check it can release a held
change after a complete BENIGN investigation. SUSPICIOUS and HOSTILE leave the hold
for the owner, who may use `tripwire release <incident> --owner`. If the resolver
exhausts its configured providers, reaches the daily limit or runs out of time,
Tripwire records NEEDS REVIEW with each observed change, any fresh observation and
the investigation outcome. A subsequent adverse report is retained and escalated.
Incomplete replies are preserved alongside the terminal report.

The next scheduled check after `hold_timeout_min` (default 60) resumes stopped processes
and jobs when an investigation remains unresolved. Access and guard files stay put back.
If the last investigator exits early without a complete verdict, the same policy applies
at the next check. With a fallback configured, the bundled primary investigator receives at most half the time
budget. A configured fallback may run once before the deadline. Tripwire verifies that
previously loaded jobs are back in launchd. A failed restoration remains recorded as held.
Automatic recovery tries the outstanding operations at most three times, then notifies the owner.
An explicit owner release can retry after the underlying fault is fixed. These checks
also cover incidents that have no holds. At most `max_summons_per_day` investigator
processes (default 4) start per Mac, including configured fallback launches.
`"hold": false` disables holds.

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
