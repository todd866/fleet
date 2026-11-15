# The stack

Fleet began as tools for running several AI coding agents on several Macs. It is becoming the
whole working setup of one person learning and building in something as complex as medicine:
the machines and agents, the data and its backups, the daily check that says what broke, the
learning tools, the media, and a way to bring a new or replacement Mac into all of it. The aim
is that someone else can copy the lot, and that a lost laptop costs a few hours of waiting.

Put another way, fleet is the recipe for a free, AI-first operating system: the agents are the
users that do most of the work, and the rest (scheduling, guards, data placement, backup, the
daily check) is what lets them work unattended without losing anything. It runs on macOS today,
and nothing in the design needs macOS. The platform-specific parts are kept small and named:
launchd (systemd timers on Linux), FSEvents (inotify), cloud placeholders (macOS's dataless flag),
disk images; the backup client already runs on Linux, in a VM on the Mac.

Each layer below says where it lives and how far it has got. "Here" means this repository.

| Layer | What it does | Where | State |
|---|---|---|---|
| Machines and agents | Shared slots for heavy work, cross-Mac agent mail, work claims, UTC schedules, stale git locks | here: [heavy-slot](../heavy-slot/README.md), [agent-mail](../agent-mail/README.md), [cowork](cowork.md), [utc-daily](../bin/utc-daily), [git-stale-locks](../bin/git-stale-locks) | in daily use |
| Guards | Rules every agent provider loads, and hooks that refuse destructive commands where agents run without prompts | here: [agentkit](../agentkit/README.md) | in daily use |
| Tripwires | Watch login keys, startup files, launchd jobs, listeners, accounts; pause what changed and start an agent to find out why | here: [tripwire](../bin/tripwire) | in daily use |
| Data placement | Which Mac or drive holds the master of each data set, what is re-obtainable, what may leave a laptop | here: [placement](../placement/README.md), [diskmap](../diskmap/README.md), [storage policy](storage-policy.md) | in daily use |
| Off-site backup | IDrive's Linux client in a small VM on the Mac that holds the drives; one copy of each thing, daily; laptops push their home to that Mac ([home-mirror](../bin/home-mirror)) instead of running a backup app; a coverage audit proves every drive and folder is backed up or skipped for a stated reason | here: [idrive-ingest](idrive-ingest.md) | first seed running (Nov 2026) |
| Morning check | One digest at the start of the day: disks, backups, jobs, sync, agent capacity, each project's own check | here: [fleet morning](morning.md) | in daily use |
| Learning | Spaced repetition scheduled over an embedding of concepts; Anki kept in step; teaching decks | [cohort](https://github.com/todd866/cohort), [anki-harness](https://github.com/todd866/anki-harness), [carousel](https://github.com/todd866/carousel), [DeckKit](https://github.com/todd866/DeckKit) | public apps; "run your own copy" guides still to write. A local-first paper library is embryonic |
| Media players and feeds | Music and TV players whose algorithmic feeds run over your own library and learn from your own signals (see below) | [Glancebar](https://github.com/todd866/glancebar), [Onto the TV](https://github.com/todd866/onto-the-tv) | in daily use by the author |
| Instruments | Menu-bar vitals, home energy, weather and aviation | [Glancebar](https://github.com/todd866/glancebar), [Energybar](https://github.com/todd866/energybar), [Isobar](https://github.com/todd866/isobar) | in daily use |
| Joining a Mac | Open a new Mac, point Claude at this repository, wait: tools, guards, jobs, agent setup, then each data set restored from its master (home drives, IDrive, GitHub) | here: [fleet-join](../bin/fleet-join), [setup](setup.md) | join and drift check working (one recipe, run on every Mac); data restore and a proven IDrive restore not built yet |

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

## Media players and algorithmic feeds

The feeds are the part most people already let a platform run for them. Here each one is a
program you own: it ranks your own library, learns from what you play, skip and finish, and runs
on your own machines, offline when it has to.

- **Music ([Glancebar](https://github.com/todd866/glancebar)).** A lossless player in the menu
  bar. The laptop holds a capped hot set of what is likely to play soon; the masters live on the
  home hub's drives and a copy in a private cloud bucket, fetched when needed. A scout finds and
  acquires new music unasked, a daily new-music radio fills its own folder, and likes from other
  services feed one ledger. Placement pins the hot set and the radio to the laptop.
- **TV ([Onto the TV](https://github.com/todd866/onto-the-tv)).** A few numbered channels fed by
  the videos you already have. Each channel picks its next episode from your library, learns from
  skips and completions (on the TV remote as well as in the app), avoids recent repeats, keeps
  separate taste per viewer, and plays on a Samsung TV on the same network.

Music and TV are what the author uses every day. Further out, and embryonic:
[omnifeed](https://github.com/todd866/omnifeed) (one feed for posts, articles, papers and podcasts,
with rebuilt headphones as the player) and a local-first paper library. They are ideas with a
storage foundation, not something to install yet.

They share the rest of the stack: media lives where placement says, the backup takes one copy,
heavy fetches and transcodes take a heavy slot, and each reports into the morning check
(Glancebar's media pipeline check covers likes, the scout, the hot set, the radio and the TV
uploads).

## Joining a new Mac, or replacing a lost one (target)

1. Sign in to the Apple ID, install the agent CLI, clone this repository.
2. Point the agent at it: it installs the tools and guards, writes the Mac's config from the
   fleet's registry, loads the jobs that belong on this Mac, and joins agent mail and sync.
3. It restores each data set this Mac keeps, from its master: the home hub's drives when on the
   home network, IDrive otherwise, GitHub for code.
4. The morning check shows the new Mac; nothing is "done" until its rows are green.

[fleet-join](../bin/fleet-join) does step 2 now: a recipe of ordered check/apply steps
([example](examples/join.json)) that installs what is missing, one step per job the registry gives
that Mac, and names the steps only a person can do (a password, a sign-in) instead of attempting
them. Run without `--apply` it changes nothing and lists drift, so the same recipe keeps the Macs
already in the fleet honest. Step 3, restoring each data set from its master, and a tested
restore from IDrive are the next pieces.
