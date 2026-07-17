#!/usr/bin/env bash
set -euo pipefail

bin="${VHS_RS_BIN:-vhs-rs}"
stem="${1:-repl-demo}"
timeline="${stem}.jsonl"
gif="${stem}.gif"
text="${stem}.txt"

{
  printf 'Type "echo repl driver"\n'
  printf 'Enter\n'
  printf 'Wait\n'
  printf 'Screen\n'
} | "$bin" repl --record "$timeline"

"$bin" render "$timeline" -o "$gif" -o "$text"
printf 'wrote %s, %s, %s\n' "$timeline" "$gif" "$text"
