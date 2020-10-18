#!/bin/bash

set -euo pipefail

./package.sh "$@"
rm itm.txt
openocd -f openocd.cfg \
  -c "tpiu config internal itm.txt uart off 16000000" \
  -c "itm port 0 on" \
  -c "itm port 1 on" \
  -c "itm port 8 on" \
  -c "program target/packager/combined.srec verify reset" \
  -c "sleep 2000" \
  -c "halt; exit"

# Extract outputs
ITM0=$(itmdump -f itm.txt -s 0)
ITM1=$(itmdump -f itm.txt -s 1)
ITM8=$(itmdump -f itm.txt -s 8)

RESULT=$(echo "$ITM8" | grep '^done ')

if [[ "$RESULT" == *pass ]]; then
    echo "TESTS PASSED :-)"
else
    echo "*** SOME TESTS FAILED ***"
    echo "$ITM8" | grep '^finish FAIL'
    echo "*** KERNEL OUTPUT ***"
    echo "$ITM0"
    echo "*** LOG OUTPUT ***"
    echo "$ITM1"
    exit 1
fi
