# The stack

Fleet began as tools for running several AI coding agents on several Macs. It is becoming the
whole working setup of one person learning and building in something as complex as medicine:
the machines and agents, the data and its backups, the daily check that says what broke, the
learning tools, the media, and a way to bring a new or replacement Mac into all of it. The aim
is that someone else can copy the lot, and that a lost laptop costs a few hours of waiting.

Each layer below says where it lives and how far it has got. "Here" means this repository.

| Layer | What it does | Where | State |
|---|---|---|---|
| Machines and agents | Shared slots for heavy work, cross-Mac agent mail, work claims, UTC schedules, stale git locks | here: [heavy-slot](../heavy-slot/README.md), [agent-mail](../agent-mail/README.md), [cowork](cowork.md), [utc-daily](../bin/utc-daily), [git-stale-locks](../bin/git-stale-locks) | in daily use |
| Guards | Rules every agent provider loads, and hooks that refuse destructive commands where agents run without prompts | here: [agentkit](../agentkit/README.md) | in daily use |
| Tripwires | Watch login keys, startup files, launchd jobs, listeners, accounts; pause what changed and start an agent to find out why | here: [tripwire](../bin/tripwire) | in daily use |
| Data placement | Which Mac or drive holds the master of each data set, what is re-obtainable, what may leave a laptop | here: [placement](../placement/README.md), [diskmap](../diskmap/README.md), [storage policy](storage-policy.md) | in daily use |
| Off-site backup | IDrive's Linux client in a small VM on the Mac that holds the drives; one copy of each thing, daily, judged by the morning check | here: [idrive-ingest](idrive-ingest.md) | first seed running (Nov 2026) |
| Morning check | One digest at the start of the day: disks, backups, jobs, sync, agent capacity, each project's own check | private tree, `fleet/morning` | in daily use; publishing once its site values are in config |
| Learning | Spaced repetition scheduled over an embedding of concepts; Anki kept in step; teaching decks; a local-first paper library | [cohort](https://github.com/todd866/cohort), [anki-harness](https://github.com/todd866/anki-harness), [carousel](https://github.com/todd866/carousel), [DeckKit](https://github.com/todd866/DeckKit); a local-first paper library (private for now) | public apps; "run your own copy" guides still to write |
| Media | A menu-bar music player with a lossless hot set and a cloud cold tier; an offline family shuffle that plays on the TV | [Glancebar](https://github.com/todd866/glancebar), [Onto the TV](https://github.com/todd866/onto-the-tv), [omnifeed](https://github.com/todd866/omnifeed) | in daily use |
| Instruments | Menu-bar vitals, home energy, weather and aviation | [Glancebar](https://github.com/todd866/glancebar), [Energybar](https://github.com/todd866/energybar), [Isobar](https://github.com/todd866/isobar) | in daily use |
| Joining a Mac | Open a new Mac, point Claude at this repository, wait: tools, guards, jobs, agent setup, then each data set restored from its master (home drives, IDrive, GitHub) | here, [setup](setup.md) | pieces exist; the one-step join and a proven restore are not built yet |

## How the layers meet

- **The morning check is the spine.** Every layer reports into it: free space and growth per
  disk, the age of the last good backup on each Mac, launchd jobs that failed, sync conflicts,
  agent capacity, and a check each project supplies (`.fleet/check`). A layer that cannot say
  whether it worked is not finished.
- **Placement decides, backup follows.** A data set has one master. The home hub's drives hold
  the masters of large personal data; IDrive holds one copy of each master, not of every mirror;
  re-obtainable media is backed up too, but never at the cost of personal data.
- **Agents are trusted to work, not to be careful.** Guards refuse the few things that cannot be
  undone, tripwires catch the rest, and both send an agent to investigate rather than a person.
- **Media and learning run on the same machines.** Music, the TV shuffle and the learning apps
  share the slots, the drives and the backup with the agents, and report into the same check.

## Joining a new Mac, or replacing a lost one (target)

1. Sign in to the Apple ID, install the agent CLI, clone this repository.
2. Point the agent at it: it installs the tools and guards, writes the Mac's config from the
   fleet's registry, loads the jobs that belong on this Mac, and joins agent mail and sync.
3. It restores each data set this Mac keeps, from its master: the home hub's drives when on the
   home network, IDrive otherwise, GitHub for code.
4. The morning check shows the new Mac; nothing is "done" until its rows are green.

Today steps 1, 2 (by hand, with [setup](setup.md)) and 4 exist. The one-step join and a tested
restore from IDrive are the next pieces.
