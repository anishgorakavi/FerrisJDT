@AGENTS.md

## Claude Code specifics

* Follow the `personal-code-hygiene` skill when writing or reviewing code here; it matches the Style
  section above (TREE.md upkeep, file headers, a comment on every function, spaces not tabs).
* Prefer the dedicated tools (Read, Edit, Grep) over shell equivalents. Do the work yourself; do not
  spawn subagents for a task this size.
* Put scratch files (probe programs, LSP harness scripts, javac output) in the session scratchpad,
  not in the repo or `/tmp`. A throwaway `javac` fixture or Node LSP driver is the quickest way to
  reproduce an editor-visible bug before writing the Rust test.
* Never claim a fix works until you have run the tests that cover it and, for editor-visible
  behaviour, exercised the rebuilt `target/release/ferrisjdt` over LSP. Report what you ran and what
  you could not verify.
* Do not use broad process kills (`pkill -f ...`); stop only processes you started.
