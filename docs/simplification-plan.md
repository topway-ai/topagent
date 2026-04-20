# Simplification Plan — Phase 2 (internal)

Internal scaffolding. Delete once the phase lands.

Previous phase removed `plans` artifact and `save_plan` tool. This phase targets
four remaining structural debts.

## Problems, ordered by leverage

1. **`observations` duplicate the metadata already in trajectories.** Observation
   records (~522 lines of code, 2 CLI commands, 5 enum variants for source kind)
   record what a promotion produced and its trust class. The same metadata lives
   in the trajectory/lesson/procedure artifacts themselves. Observation is a
   CLI-only index over promotions that never feeds back into any decision. Highest
   deletion leverage this phase.

2. **`learning.rs` is a 1069-line dispatch dumping ground.** Four unrelated command
   families (memory, procedure, trajectory, observation) plus rendering helpers
   plus test fixtures live behind one `mod learning`. Each domain already has a
   module under `memory/`. Dissolving `learning.rs` puts CLI rendering next to
   the data it renders.

3. **main.rs still owns all clap type definitions.** After the dispatch was
   thinned, the enum types (Commands, ServiceCommands, ModelCommands, etc.)
   stayed in main.rs. Moving them to a `commands/types.rs` makes main.rs
   genuinely boring: parse + one dispatch call.

4. **User-facing contract drifts across four surfaces.** README, clap help,
   Telegram /help, and docs/operations.md each restate command surface, flags,
   and artifact taxonomy. README has a duplicate `### Bot commands` header.
   README says `--max-retries` default is 3 (code says 10). README says provider
   is auto-detected from model ID (code now uses explicit provider selection).
   No mechanism catches future drift.

5. **status / doctor / config inspect / run status overlap.** Distinct intents
   on paper, but operator-facing framing is not sharp. Not deletable — needs a
   single paragraph that names each lane.

## Deletion targets this phase

### Remove `observations` artifact type

Observations are metadata about promotions. The same metadata is in the promoted
artifacts (lessons have verification, procedures have verification, trajectories
have verification + trust labels + tool sequence). Removing observations:
- Deletes `memory/observation.rs` (~522 lines)
- Removes 2 top-level CLI commands (`observation list`, `observation show`)
- Removes ObservationCommands enum and Observation variant from Commands
- Removes observations dir from WorkspaceMemory and ensure_layout
- Removes emit_observation calls from promotion.rs
- Removes ~30 lines of rendering from learning.rs
- Removes observation count from memory status output

Operator visibility is preserved: `memory status` still shows artifact counts
for lessons, procedures, trajectories. Individual artifact `show` commands
already display verification and trust metadata.

Migration: `.topagent/observations/` directories left on disk. Doctor no longer
reports them. Next consolidation prunes any MEMORY.md entry pointing at
`observations/`. No automated deletion.

## Workstream A: dissolve `learning.rs` into per-domain modules

Move each dispatch function next to its domain. No new types.

| Current                        | Destination                                           |
| ------------------------------ | ----------------------------------------------------- |
| `run_memory_command`           | new `memory/cli.rs` (status + lint + recall)          |
| `run_procedure_command`        | `memory/procedures.rs` (add `pub(crate) fn run_cli`)  |
| `run_trajectory_command`       | `memory/trajectories.rs` (add `pub(crate) fn run_cli`)|
| `migrate_profile_if_needed`    | `memory.rs` (workspace migration, not a command)       |
| render/list helpers            | move into the domain module that owns the data        |
| generic file helpers           | `memory.rs` if shared, inline if used once            |

Also move `checkpoint.rs` content into `commands/checkpoint.rs` (checkpoint
command rendering, not the hot-path checkpoint store).

## Workstream A2: move clap types + dispatch out of main.rs

1. Create `commands/types.rs` — all clap enum types (Commands, ServiceCommands,
   ModelCommands, MemoryCommands, ProcedureCommands, TrajectoryCommands,
   CheckpointCommands, ConfigCommands, RunCommands, ToolAuthoringMode)
2. Create `commands/dispatch.rs` — the top-level match + run function
3. main.rs becomes: init_tracing + Cli::parse + dispatch::run(cli)

After this, "where does top-level dispatch stop?" answer: `commands/dispatch.rs`.

## Workstream B: single-source truth for operator contract

1. Fix factual errors: README `--max-retries` default 3→10, provider wording.
2. Fix duplicate `### Bot commands` header in README.
3. Add a `cli_docs_consistency` test that asserts each subcommand in clap help
   appears in the README command table, and vice versa.
4. Add authoritative-source note to docs/operations.md: "Command surface is
   authoritative in `topagent --help`; this document explains intent, not flags."
5. Telegram `/start` help: pull command table into a const; add unit test
   cross-checking advertised commands vs router-handled commands.

## Workstream C: memory taxonomy after observation deletion

```
workspace/.topagent/
  USER.md               operator preferences (operator-only, loaded separately)
  MEMORY.md             always-loaded tiny index
  topics/               compact durable notes by concern (lazy loaded)
  lessons/              distilled facts/pitfalls/rules from verified runs
  procedures/           reusable playbooks from verified reuse (lazy loaded)
  trajectories/         structured export traces (review→export workflow)
  exports/trajectories/ reviewed trajectory packages
  telegram-history/     per-chat transcript evidence
  checkpoints/          automatic workspace checkpoints
  tools/                generated tools
  hooks.toml            hook config
  external-tools.json   external tool config
```

Four live durable categories (topics, lessons, procedures, trajectories), plus
operator/index/evidence/checkpoint. Down from five when `observations/` existed.

## Workstream D: lifecycle command lanes

Add a "Diagnostic commands" section to docs/operations.md:

| Command            | Answers                                                                       |
| ------------------ | ----------------------------------------------------------------------------- |
| `status`           | Is TopAgent installed? Is the service up? Is a default model set?             |
| `doctor`           | Are prerequisites, paths, permissions, and tool resolution healthy right now? |
| `config inspect`   | What runtime contract will the next run use — provider, model, keys, options? |
| `run status`       | Is there a recoverable execution session — checkpoint, transcripts, service?  |

No behaviour change. `install`/`setup` remain aliases. `uninstall` with/without
`--purge` keep distinct semantics. Just a contract paragraph.

## Execution order

1. Write this note. ← current step.
2. Remove `observations` across both crates + docs + tests.
3. Move clap types + dispatch out of main.rs.
4. Dissolve `learning.rs` into per-domain modules.
5. Move `checkpoint.rs` command rendering into `commands/checkpoint.rs`.
6. Fix doc drift (README errors, duplicate header, authoritative note).
7. Add cli_docs_consistency test.
8. Add Telegram help/router cross-check test.
9. Add lifecycle-lanes section to docs/operations.md.
10. `cargo test --workspace`.
11. Final report.

## Non-goals this phase

- No provider changes.
- No new artifact types.
- No new subagent, marketplace, remote storage.
- No touch on approvals, trust-context labels, compaction, transport seam.
- No renaming of existing CLI commands (observation removed, not renamed).
