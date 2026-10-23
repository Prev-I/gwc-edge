# rs-gcache

Instructions for all AI coding agents (Claude Code, Codex, OpenCode).

## Project Overview

The repository is newly created and holds no code yet. There is no build, no
test suite and nothing executable. When code arrives, record here the commands
to build, test and lint it, and the conventions that are not obvious from the
code itself.

## Conventions

- Code, commit messages, branch names and documentation in **English**.
- Conventional commits: `feat:`, `fix:`, `docs:`, `chore:`, `refactor:`,
  `test:`. Keep commits atomic.
- Integration follows `.repository-policy.yaml`: work goes on a branch and
  reaches `main` through a pull request.
- `AGENTS.md` is the only place agent guidance is written; `CLAUDE.md` stays a
  one-line `@AGENTS.md` import.
- No secrets, tokens, API keys or connection strings in any committed file.
