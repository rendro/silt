#!/bin/bash
cd /home/rendro/dev/silt
git add programs/ docs/friction-report.md
git commit -m "Add 10 evaluation programs and friction report

Programs covering: link checker, CSV analyzer, concurrent processor,
key-value store, expression evaluator, todo manager, text statistics,
config parser, pipeline transformer, and test framework.

Friction report synthesizes findings from all 10 programs:
- Overall rating: 6.0/10
- Top gaps: list.append, list.get(index), string.slice
- Top strengths: pipes, trailing closures, pattern matching

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
git push
echo "Done"
