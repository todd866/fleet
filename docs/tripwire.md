# tripwire

Prevention on these Macs is deliberately light: agents run without permission prompts and
unattended jobs must restart by themselves. `tripwire` is the other half. It watches the few
things that have to change for someone to get in, stay in, or switch the guards off, and when
one changes it starts an agent to find out why.

| Wire | Raises when |
|---|---|
| `files` | a login, shell-startup, sudo, ssh or guard-config file changes (`files` in the config); so do tripwire's own config and programs, the Python that runs them and the investigator's provider program, which are always watched |
| `guards` | a provider's hook file stops calling the guard, or calls it from somewhere else (`must_contain`) |
| `tripwire` | part of tripwire's own saved state is missing or does not carry its seal |
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

| Change | Hold | Release | Who releases |
|---|---|---|---|
| new listener | its process is stopped (`SIGSTOP`) | continued (`SIGCONT`), if it is still the same process | the investigator's BENIGN, or the owner |
| new or changed job in `~/Library/LaunchAgents` | disabled, and unloaded if it was loaded | enabled, and loaded again if it had been and its plist is still the one that was held | the investigator's BENIGN, or the owner |
| login keys, a guard hook file, tripwire's config (`hold_files`, `must_contain`) | put back to the last checked version, or taken away if there was none; the changed version is kept in the incident's `held/` | the changed version returns, unless the file changed again meanwhile | the owner only |

A guard hook file that is exactly what `agentkit sync` would write from the owner's own site
config (its dry run reports no difference for that file) is not held: a guard update rewrites
every hook file on every Mac, and that needs no owner. Both the site config and the agentkit
binary are files the guard refuses agent writes to. A hook file that differs from what agentkit
generates, or a sync that cannot run, is held as above (owner, 28 Nov 2026).

Other watched files, system-wide jobs and accounts are investigated but not held: holding them
needs root or would undo ordinary edits. The job that runs `tripwire` is never held, nor are
the programs in `never_pause`. That job is known by its label, pinned when the owner accepts
the baseline, or by launchd confirming that the running check is that job: a plist does not
earn the exemption by mentioning tripwire.

A guard hook counts as wired only when a hook's whole command is the guard's own
(`agentkit hook …` with plain arguments). `true # agentkit hook`, `agentkit hook … || true`,
the same command moved to another event or matcher, and `disableAllHooks` are all a lost guard.

## What a verdict may release

A BENIGN verdict lets work carry on: it continues a stopped process and enables a disabled job.
It never returns a put-back file. The investigator runs as the owner: its provider program sits
in a folder the owner's own programs can write, its sign-in and settings are the owner's files,
and the model reads evidence whose names and contents the author of the change chose. That is
good enough to decide whether a dev server may keep running. It is not good enough to decide
who may log in to this Mac or whether the command guards stay wired in. Those files come back
only when the owner runs `tripwire release <incident> --owner`.

## Owner only

`tripwire accept` and `tripwire release <incident> --owner` run only at a terminal, after
`sudo` has taken the owner's password or Touch ID with its cached ticket ignored (nothing is
run as root; a Mac where sudo asks for nothing cannot use them). A terminal alone proves
nothing, since agents have pseudo-terminals, and neither does the flag. Over ssh: `ssh -t HOST
tripwire accept`. An unattended install does not need `accept`: list the job in the registry
first and tripwire raises nothing for it.

## What tripwire believes

Only what it wrote. The baseline, each kept copy, each incident record, each report, each
investigation receipt and the count of agents started carry an HMAC-SHA256 seal. Something
without a valid seal is evidence, never an instruction: nothing is released, put back, loaded
or restarted on its word, and tripwire raises an incident on the `tripwire` wire, which goes to
the owner because no investigator can vouch for tripwire's own state. A sealed hold is still
undone only where the configuration allows: a file among the hold files, from the copy in that
incident's own folder, unchanged since it was saved; a job whose plist sits in a launchd folder
of this user's under the recorded label.

The key is 32 random bytes in `~/.local/state/agentkit/tripwire.key`: created once, exclusively,
and used only while it is a plain file of this user's, mode 0600, in a folder nobody else can
write. agentkit's guard already refuses agents' writes to that folder. **It must also refuse
their reads of the key**: add the path to `secret_paths` in the guard's configuration. Run with
`TRIPWIRE_STATE`, `TRIPWIRE_CONFIG` or another `HOME` and tripwire uses a key inside that state
folder instead, so a run pointed at other files cannot sign for the real state.

The investigator never holds the key. `tripwire check` starts `tripwire review`, writes that
process's pid and start time into the sealed record, and only that process runs the
investigator: it hands over the evidence it has just verified on stdin (the investigator reads
no file of the incident's), takes the answer from a pipe, decides from the sealed record
whether the incident was a drill, and seals the report. A report is then read once, and its
verdict and seal are judged over the same bytes. The investigator is this checkout's own file,
run by the same Python, with a PATH of system folders; its provider program is passed by
absolute path, pinned by SHA-256 in the sealed record, checked again before use, and watched,
so a replaced provider raises an incident and is not used for the check that notices it. A
provider swapped in the moment between that check and its start is not caught; under the rule
above the most it can do is let a held job or process go on an hour early.
`tripwire-investigate` run by hand prints an answer and saves nothing.

The configuration is held like a login-key file. An edit could switch holds off or name the
investigator, so it is put back and not obeyed until the owner returns it or accepts it.

### Lockdown

When tripwire has saved state but no baseline it can believe (the baseline or the key is
missing or was rewritten, or this is the first run of a version with seals over state from one
without), nothing on the Mac is taken as found. The hold files are compared with their sealed
kept copies and put back where they differ, with or without an investigator; the kept copies,
the accepted configuration (the sealed copy, else the defaults) and tripwire's own job stay as
they were; no report releases anything; everything else is watched from a provisional baseline.
`tripwire status` says so, and it lasts until the owner runs `tripwire accept`. Only a state
folder with nothing in it and no key is a first run.

Upgrading a Mac from a version without seals is therefore the owner's step, not something
tripwire infers: check `tripwire status` for anything still held and release it with the old
version first (the new one never acts on an unsigned record), install, then run `tripwire
accept`.

Gemini runs in sandboxed plan mode. The Z.ai route sends a bounded set of typed
observations to the installed loopback proxy. To use an already configured DeepSeek
account after Z.ai fails, set the fallback command to
`["tripwire-investigate", "--engine", "zai", "--last-resort", "deepseek"]`.
Both text-only routes omit file contents, paths, process arguments and tool definitions.
Their metadata can establish a benign cause for the explicit local CLI drill. A benign
guess about an ordinary incident becomes NEEDS REVIEW. The investigator records provider
outcomes in the incident directory.

Tripwire saves complete verdicts atomically, sealed. On its next check a complete BENIGN
from its own investigator undoes the operational holds; put-back files stay for the owner.
SUSPICIOUS and HOSTILE leave every hold for the owner, who may use
`tripwire release <incident> --owner`. A summon command other than `tripwire-investigate`
writes its own `report.md`, which carries no seal: tripwire tells the owner the verdict and
releases nothing on it. If the resolver
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
processes (default 4) start per Mac, including configured fallback launches; drills
(`tripwire test --summon`) draw on an equal allowance of their own, so that running drills
cannot leave a real change without an investigator. `"hold": false` disables holds.

The cost is honest: the first time a program listens on the network, or a job is installed
outside the registry, it stops for the few minutes the agent takes. Listeners are remembered
once seen, and a job listed in the registry before it is installed is not raised at all.

`tripwire status` lists wires, incidents and holds. `tripwire accept` takes the current state
as the baseline after a change the owner made himself, including an edit to tripwire's config
or its launchd job. `tripwire test` raises a harmless incident (`--summon` also starts the
agent). Install: link `bin/tripwire` into `~/.local/bin`, load
[launchd/com.example.tripwire.plist](launchd/com.example.tripwire.plist), then run `tripwire
accept` once so that the job's label is pinned. The example job runs tripwire with the system
Python by absolute path and puts the system folders first on PATH: a `python3` or `lsof` planted
in a folder the owner's programs can write must not be what tripwire runs. Config:
`~/.config/fleet/tripwire.json`, every key optional (see the top of `bin/tripwire`).

What it does not see: anything that changes nothing on this list (a secret read and sent out,
a process that never listens), and a change made and reverted between two checks.

What the seals do not stop. They stop anything that can only write files, or only run
tripwire's own commands, from steering tripwire. They do not stop a program running as the
owner that is prepared to run code of its own: it can read the key (the guard refuses the
obvious ways, not every way), load tripwire's functions, or edit tripwire's programs, which
are watched but run before the watch reports. Such a program needs no help from tripwire to
write a login-key file, so the seals take nothing from it and give it nothing; what they end
is tripwire doing that writing for something that could not. An older sealed baseline put back
in place of the current one is also not detected. A hard boundary needs the key and the state
out of the owner's reach (a root-owned daemon or a separate account). It is a tripwire, not a
monitor.
