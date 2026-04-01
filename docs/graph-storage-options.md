# Graph Storage Alternatives

## Current state

Tempyr's code currently supports a separate knowledge-graph repository via `.tempyr-redirect`, but the main spec still models the graph as files inside the project under `graph/` with derived state in `.tempyr/`. That is an important distinction:

- The external-repo workflow is an implementation escape hatch, not the architectural center of gravity.
- The current Rust code is still heavily file-and-directory based. `Graph::load_from_directory`, node ops, migration code, MCP handlers, and interview commit paths all read and write real files under a `graph/` directory.

That means "move back to tracked files in the repo" is a much smaller change than "teach the whole stack to operate on abstract Git objects instead of files."

## Constraints that matter

Any replacement for the current external-repo setup needs to satisfy:

1. Branch-local knowledge must stay branch-local until merge.
2. Rollback must be easy and Git-native.
3. It must behave well with linked worktrees.
4. Large derived state, especially the vector index, should not be duplicated blindly per worktree.
5. Onboarding for a new project should be close to `tempyr init`, not "create and coordinate a second repo."

## Option 1: Track the graph in the main repo

### Shape

- Keep `graph/` and tracked `.tempyr/schema.toml` in the project repository.
- Keep mutable derived state gitignored.
- Treat knowledge edits exactly like code edits on the current branch.

Suggested layout:

```text
repo/
|-- .tempyr/
|   |-- schema.toml
|   |-- config.toml
|   `-- sessions/              # gitignored
|-- graph/                     # tracked
`-- .git/
    `-- tempyr/
        |-- indexes/
        `-- embeddings/
```

The key refinement is that the heavy derived cache should move out of the worktree and into Git's common directory, discovered via `git rev-parse --git-common-dir`, not stay as `.tempyr/index.db` inside every checkout.

### Why this fits the constraints

- Branch behavior is automatic. If graph files live in the same repo as code, knowledge follows the branch and only reaches `main` when the branch is merged.
- Rollback is automatic. `git revert`, `git checkout <old-commit> -- graph/`, and ordinary branch history all work.
- Worktrees are mostly solved by Git already. Linked worktrees share refs and the common Git object store; only checkout state is duplicated.
- The graph files themselves are plain text and usually cheap compared to the derived index.

### Worktree/index strategy

Use a shared cache under Git's common dir, for example `<git-common-dir>/tempyr/`, keyed by graph content:

- `indexes/<graph-tree-oid>/index.db`
- `indexes/<graph-tree-oid>/meta.json`
- `embeddings/<body-hash>.bin`

This gives the right semantics:

- Two worktrees on the same graph snapshot reuse the same index.
- A feature branch with different graph files gets a different cache key.
- If a branch never merges, its cache can be garbage-collected later.

The existing content-hash embedding model already points in this direction. Per-body embeddings should be shared globally across worktrees; only the structural SQLite index needs to be reassembled per graph snapshot.

### Hook strategy

Use a managed Git `post-checkout` hook to create or warm the cache on:

- branch switch
- checkout
- `git worktree add` with checkout

I verified locally on Git `2.53.0.windows.2` that `git worktree add` triggers `post-checkout` in the new worktree when checkout occurs.

Also useful:

- `post-merge` to refresh cache after merges
- `post-commit` only if you want eager index refresh for graph-only commits

Do not hardlink a mutable `index.db` between worktrees. That would make multiple paths refer to the same database file. Use one of:

- direct reuse when the cache key matches exactly
- copy-on-write clone / reflink when the filesystem supports it
- plain file copy as a fallback

Hardlinking is fine for immutable Git objects; it is the wrong primitive for a mutable SQLite database.

### Pros

- Lowest conceptual complexity
- Best match to the current codebase
- Best branch semantics
- Best UX for new projects
- Normal Git diff, merge, review, and blame continue to work

### Cons

- Graph files appear in the working tree, which some users may consider clutter
- Large graphs still increase checkout size
- You may eventually want custom merge assistance for sorted edge lists and YAML frontmatter

## Option 2: Store the graph in hidden refs inside the main repo

### Shape

Store each knowledge snapshot as a Git tree/commit under a custom ref namespace such as:

- `refs/tempyr/heads/<branch>`
- or `refs/namespaces/tempyr/refs/heads/<branch>`

Tempyr would read and write the graph using Git plumbing instead of filesystem paths.

### Can Git do this?

Yes. Git refs can point at commits, commits point at trees, and trees represent complete file hierarchies. So the basic mechanism is sound.

In practice, this means Tempyr would need to:

- materialize or parse files from tree objects
- create blobs/trees/commits for graph updates
- move refs explicitly
- manage reflogs for rollback
- map code branches to Tempyr refs
- merge Tempyr refs when code branches merge

### Why this is attractive

- No graph checkout clutter in the worktree
- The graph history can stay logically separate from code history
- All data still lives inside the same Git repository and shares the same object store

### Why this is expensive

- The current codebase is deeply filesystem-oriented. A hidden-ref backend is not a path tweak; it is a storage abstraction project.
- Normal Git UX goes away unless you rebuild it: status, diff, merge conflict handling, ad hoc editing, review ergonomics.
- Hidden refs require explicit fetch/push refspecs.
- Hidden refs are not a security boundary. Git's own docs say hiding refs is only an advertisement control and private data is best kept in a separate repository.

### Best use of this option

Treat it as an advanced backend, not the default. It is feasible, but it should come after Tempyr has a clear `StorageBackend` abstraction.

## Option 3: Keep a separate Git store, but co-locate it under the main repo

### Shape

Instead of asking users to manage a totally separate repo by hand, Tempyr can create a sidecar Git repository under the main repo's common Git directory, for example:

- `$GIT_COMMON_DIR/tempyr.git` as a bare repo
- or a repo created with `git init --separate-git-dir`

Then map code branches to graph branches internally.

### Why this is better than today's redirect model

- It removes most user-facing friction
- It works naturally with linked worktrees because all worktrees share `$GIT_COMMON_DIR`
- It preserves the idea of separate graph history if that is important

### Why it is still second-best

- It is still logically a second repository
- Branch synchronization is still Tempyr's problem
- Merges still need coordination across two histories
- It is more complex than simply tracking the graph files with code

This is a good compromise if separate history is a hard requirement. It is not the best default if the real goal is to reduce friction.

## Options to reject

### Git submodules

This keeps the exact class of friction you want to remove: separate lifecycle, separate branch management, separate fetch/push behavior.

### Git notes as the primary store

Git notes are for attaching extra data to existing Git objects, usually commits. They are useful for annotations, not as the main representation of an independent knowledge graph with its own node/edge file tree.

## Recommended direction

### Recommendation

Make tracked in-repo graph files the default backend again, and move heavy derived state into a shared cache under Git's common dir.

That gets you:

- branch-local knowledge for free
- rollback for free
- worktree support with minimal extra machinery
- much lower onboarding friction
- the smallest implementation delta from the current codebase

### Concrete design

1. Default backend: `worktree_files`
   - `graph/` is tracked in the main repo.
   - `.tempyr/schema.toml` and `.tempyr/config.toml` are tracked.
   - `.tempyr/sessions/` stays gitignored.

2. Shared cache in Git common dir
   - Resolve it with `git rev-parse --git-common-dir`.
   - Add `cache_dir = <git-common-dir>/tempyr`.
   - Compute cache keys from the graph tree OID plus relevant config hash.
   - Share embeddings by content hash across all worktrees.

3. Hook support
   - Install repo-managed Git hooks with `core.hooksPath`.
   - Use `post-checkout` and `post-merge` for cache warmup/refresh.
   - Optionally enable `extensions.worktreeConfig` if any Tempyr config should vary by worktree.

4. Backward compatibility
   - Keep `.tempyr-redirect` as a legacy or advanced backend.
   - Add a migration command that imports from the external repo into in-repo tracked files.

5. Later, if needed
   - Add an experimental `git_ref` backend after introducing a storage abstraction.
   - Keep it opt-in until merge/push/review UX is good.

## Suggested implementation sequence

1. Introduce `cache_dir` and resolve it via the Git common dir.
2. Move index and embedding cache lookup away from worktree-local `.tempyr/index.db`.
3. Make `tempyr init` default to in-repo tracked storage only.
4. Add a managed hook installer for cache warmup.
5. Add `tempyr migrate storage` to move redirected projects back into the main repo.
6. Only then evaluate whether a `git_ref` backend is still worth the complexity.

## References

- Git worktrees: refs are shared across worktrees; per-worktree config is available via `extensions.worktreeConfig`.
  - https://git-scm.com/docs/git-worktree
  - https://git-scm.com/docs/git-config
- Git repository layout and alternate object stores:
  - https://git-scm.com/docs/gitrepository-layout
- Separate Git directories:
  - https://git-scm.com/docs/git-init
- Hidden refs and the warning that hidden refs are not a true privacy boundary:
  - https://git-scm.com/docs/git-config
  - https://git-scm.com/docs/gitnamespaces
- Git notes are object annotations, not a general file-tree store:
  - https://git-scm.com/docs/git-notes
- Git tree/commit plumbing used by a future hidden-ref backend:
  - https://git-scm.com/docs/git-commit-tree
  - https://git-scm.com/docs/git-update-ref
