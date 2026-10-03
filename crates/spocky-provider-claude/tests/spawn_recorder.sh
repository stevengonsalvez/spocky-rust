#!/bin/sh
# Stands in for the Claude binary: records its argv, working directory,
# environment and the lines written to its stdin, then exits after three
# seconds. The paths come from RECORD_FILE and RECORD_STDIN.
{
  echo "ARGC $#"
  for argument in "$@"; do printf 'ARG %s\n' "$argument"; done
  echo "PWD $(pwd)"
  echo "ENV-BEGIN"
  env
  echo "ENV-END"
} > "$RECORD_FILE"
: > "$RECORD_STDIN"
( sleep 3; kill $$ ) > /dev/null 2>&1 &
while IFS= read -r line; do printf '%s\n' "$line" >> "$RECORD_STDIN"; done
