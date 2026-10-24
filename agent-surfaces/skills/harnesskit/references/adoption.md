# Existing-document restructuring

Use this bounded workflow when adopting HarnessKit in a repository that already
has a README, handbook, notes, or another documentation layout.

1. Inventory the existing entrypoints, indexes, focused documents, and repeated
   passages. Record the source path and the intended destination for every
   passage that will move.
2. Write a short plan before editing: list each source passage, its destination,
   the owning collection, and any links or index entries that must change.
3. Move the passages into the existing layout when it already works. Remove the
   original passage after the move; do not copy a handbook into a new reference
   document while leaving the source text behind. Create a narrow document only
   when no existing owner fits.
4. Add only a one-line HarnessKit route to `AGENTS.md` or `CLAUDE.md`. Keep tool
   installation and HarnessKit command detail in skill references, never in
   product documentation.
5. Update only the affected indexes and links, then run `harnesskit check` and
   the repository's tests. Review the diff for duplicated paragraphs and for
   source passages that were meant to be removed.
