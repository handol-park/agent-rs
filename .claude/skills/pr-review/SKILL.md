---
name: pr-review
description: Run a code review on a PR and post inline comments. Pass a PR number as argument, e.g. /pr-review 42. Uses /code-review --comment.
allowed-tools: ["Bash", "Glob", "Grep", "Read", "Skill", "Task", "mcp__github_inline_comment__create_inline_comment"]
---

# pr-review

Run `/code-review` on a PR and post findings as inline comments.

## Steps

1. Resolve the PR number: use the argument if provided; otherwise run
   `gh pr view --json number -q .number` to get the current branch's PR.
2. Run `/code-review --comment <owner>/<repo>/pull/<number>`.
