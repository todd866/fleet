"""Open the private dashboard only when a hub is configured.

The historical launcher tunneled to a board process with a one-time ticket and
never put that ticket in the browser history. This tree does not ship that
server. Without `hub` in the cowork config, this command does not open a
network connection.
"""
import sys

from cowork.cli import main

if __name__ == "__main__":
    sys.exit(main(["open", *sys.argv[1:]]))
