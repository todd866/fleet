# IDrive Linux headless client — native arm64
# Reference: docs/idrive-ingest.md
#
# The client is a Perl application installed to /opt/IDriveForLinux, scheduled via cron,
# managed from IDrive's web console. It backs up any configured path, which is what lets
# us point it at the mirror data and (later) the encrypted drives — the macOS app cannot
# select external volumes, which is the whole reason for the Linux client.
FROM debian:bookworm-slim

# Perl client + cron scheduler + TLS + fetch/debug tools. Kept minimal; the client
# bundles its own platform binaries and perl libraries under /opt/IDriveForLinux.
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        perl cron ca-certificates curl procps iproute2 net-tools \
    && rm -rf /var/lib/apt/lists/*

# The installer is a self-extracting .bin (shell script + appended tar.gz). It is NOT
# committed (55 MB+); fetch it first with scripts/fetch-idrive-client.sh.
COPY idriveforlinux.bin /tmp/idriveforlinux.bin
RUN chmod +x /tmp/idriveforlinux.bin

# Install (accept the terms prompt with 'A'; the installer auto-installs any missing
# scheduler dependency). Leaves the client at /opt/IDriveForLinux/bin/idrive.
RUN echo "A" | /tmp/idriveforlinux.bin --install
RUN rm -f /tmp/idriveforlinux.bin

# The client bundles its own Python 3.5 and native libs under Idrivelib; the packed Perl
# binary resolves those through LD_LIBRARY_PATH, and it expects to run from its own bin dir
# (it does `stat("idrive")` relative to cwd). TERM is required for the interactive menu.
ENV LD_LIBRARY_PATH=/opt/IDriveForLinux/bin/Idrivelib/dependencies/python/lib
ENV TERM=xterm
WORKDIR /opt/IDriveForLinux/bin

# The interactive account setup (email + password + backup set + schedule) is done by the
# OWNER via `docker exec -it <name> ./idrive` so credentials never pass through an agent.
# Account config persists under /opt/IDriveForLinux/idriveIt and the schedule in root's
# crontab — bind-mount both when running so a container restart keeps the login. The client
# writes a crontab entry during setup; run cron in the foreground as the long-lived process.
# A manual backup is also available via `./idrive` → backup.
CMD ["cron", "-f", "-l", "2"]
