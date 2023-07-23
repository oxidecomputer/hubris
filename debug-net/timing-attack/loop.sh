#!/bin/bash

export HUMILITY=~mkeeter/humility/target/release/humility
export HUMILITY_ENVIRONMENT=~mkeeter/oxide/env.json
export HUMILITY_TARGET=nucleo

RING_START=$($HUMILITY readvar -l \
    |grep RX_DESC \
    |awk '{print $3}')
printf "got ring start 0x%X\n" $RING_START

while :
do
    ./target/release/timing-attack --mac "0e:1d:9a:64:b8:c2" -ienp0s25 sweep --start 750 --end 950
    $HUMILITY readmem $((0x40028000 + 0x1160)) 4 -w
    $HUMILITY readmem $((0x40028000 + 0x115c)) 4 -w
    CURRENT_DESCRIPTOR=$($HUMILITY readmem $((0x40028000 + 0x114c)) 4 -w \
        | tail -n1 \
        | awk '{print $3}'
    )
    echo "got current descriptor index ${CURRENT_DESCRIPTOR}"
    PREV_DESCRIPTOR=$(( (((0x${CURRENT_DESCRIPTOR} - $RING_START) / 16) + 3) % 4 ))
    echo "got prev descriptor index ${PREV_DESCRIPTOR}"
    PREV_ADDR=$((0x30001840 + $PREV_DESCRIPTOR * 16))
    printf "got prev descriptor addr 0x%X\n" $PREV_ADDR
    PREV_BUF=$($HUMILITY readmem $PREV_ADDR 16 -w \
        | tail -n1 \
        | awk '{print $3}'
    )
    echo "got prev buf 0x$PREV_BUF"
    $HUMILITY readmem 0x$PREV_BUF 96

    $HUMILITY reset
    sleep 10
done
