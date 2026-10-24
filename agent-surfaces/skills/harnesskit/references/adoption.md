# Existing-document restructuring

Use this bounded workflow when adopting HarnessKit in a repository that already
has a README, handbook, notes, or another documentation layout.

1. Inventory the existing entrypoints, indexes, focused documents, and repeated
   passages. Record the source path and the intended destination for every
   passage that will move. For each rule or invariant, find every file that
   states it, including notes and worksheets, by searching a distinctive phrase
   with `rg` rather than opening every file.
2. Write a short plan before editing: list each source passage, its destination,
   the owning collection, and any links or index entries that must change.
3. Move the passages into the existing layout when it already works. Remove the
   original passage after the move; do not copy a handbook into a new reference
   document while leaving the source text behind. Create a narrow document only
   when no existing owner fits.
   A note, worksheet, or index entry that only restates a rule owned elsewhere
   is a copy, not evidence: replace that text with a link to the owner, and keep
   what is unique to it, such as dates, cases, or handover detail. Index entries
   carry a title and a short purpose, not the rule itself.
4. Add only a one-line HarnessKit route to `AGENTS.md` or `CLAUDE.md`. Keep tool
   installation and HarnessKit command detail in skill references, never in
   product documentation.
5. Update only the affected indexes and links, then run `harnesskit check` and
   the repository's tests. Review the diff for duplicated paragraphs, including
   lightly reworded ones, and for source passages that were meant to be removed.
