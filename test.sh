#!/bin/bash

set -e -u -o pipefail

trap "trap - SIGTERM && kill -- -$$" SIGINT SIGTERM EXIT

EINSOCK="/tmp/einhyrningsins_tests.sock"
EIN="target/debug/einhyrningsins --socket-path $EINSOCK"
EINCTL="target/debug/einhyrningsinsctl --socket-path $EINSOCK"

export RUST_BACKTRACE=1

echo "### Run through ctl commands"
rm -f $EINSOCK
$EIN -- nc -l 127.0.0.1 9123 > /dev/null &
sleep 0.5
$EINCTL -e inc > /dev/null
$EINCTL -e upgrade > /dev/null
$EINCTL -e dec > /dev/null
$EINCTL -e status > /dev/null
$EINCTL -e help > /dev/null
$EINCTL -e version > /dev/null
$EINCTL -e die > /dev/null
sleep 1
jobs > /dev/null
test -z "`jobs`"

echo "### Graceful shutdown via ctl"
rm -f $EINSOCK
$EIN --bind localhost:9876 -- examples/einhorn_http.py > /dev/null &
sleep 0.5
$EINCTL -e die > /dev/null
sleep 1
jobs > /dev/null
test -z "`jobs`"
trap "trap - SIGTERM" SIGINT SIGTERM EXIT
