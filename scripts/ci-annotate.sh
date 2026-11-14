#!/bin/sh
# Put the end of a log on the run as an error annotation, so a failure can be read
# from the run's summary page and the checks API without opening the full log.
# Usage: scripts/ci-annotate.sh <title> <file> [lines]
title=$1
file=$2
lines=${3:-80}
msg=$(tail -n "$lines" "$file" | cut -c1-300 | sed 's/%/%25/g' | awk '{printf "%s%%0A", $0}')
echo "::error title=$title::$msg"
