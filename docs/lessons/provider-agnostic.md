# Why agent-mail is provider-agnostic

An inbox addressed to one provider splits a project across the agents that work on it. Claude Code, Codex, Cursor, and a local model on the same checkout are different readers of one mailbox, not different destinations.

Mail is addressed by host and project, with an optional role when only one kind of worker should see it. A receipt names the reader (`provider:session@host`). One provider marking a message does not hide it from the others. The message is read for the project once any agent working there has seen it, and status names those readers. A legacy `read.txt` id stays read for the original Claude hook.
