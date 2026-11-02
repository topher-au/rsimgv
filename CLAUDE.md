# Claude Code Development Guidelines

These instructions define the expected development workflow. Use good
engineering judgment: work independently on routine decisions, but be
deliberate about scope, Git safety, and expensive verification.

## 1. Understand the project before changing it

-   Inspect the repository structure, relevant source files, existing
    documentation, and established conventions before implementing
    changes.
-   Check `git status` and the recent commit history before starting.
-   Identify affected components, dependencies, interfaces, and likely
    regression risks.
-   For substantial work, create a concise implementation plan and
    divide it into logical milestones.
-   Prefer existing project patterns and dependencies over introducing
    new abstractions or tools.
-   For complex or ambiguous work, explain the plan and ask for
    clarification before making consequential assumptions.

## 2. Control scope

-   Make the smallest coherent set of changes that fully addresses the
    request.
-   Do not refactor unrelated code, reformat unrelated files, or change
    naming conventions incidentally.
-   Avoid adding dependencies, configuration options, abstractions, or
    compatibility layers without a concrete need.
-   Preserve existing APIs and behaviour unless the task explicitly
    requires changing them.
-   Keep unrelated fixes separate from feature work and from each
    other's commits where practical.

## 3. Use branches for significant work

-   Keep the repository's default branch stable.
-   For a substantial feature, architectural change, broad refactor, or
    multi-step task, create a dedicated branch before editing files.
-   Use descriptive branch names with conventional prefixes:
    -   `feat/` for features
    -   `fix/` for bug fixes
    -   `refactor/` for refactoring
    -   `perf/` for performance work
    -   `chore/` for maintenance
-   Small, isolated changes may stay on the current branch when that is
    appropriate.
-   Do not switch branches while carrying uncommitted work unless you
    have confirmed it is safe and will preserve that work.
-   Do not automatically merge, push, or open a pull request unless
    requested.

## 4. Commit meaningful progress regularly

-   Treat commits as useful checkpoints throughout development, not
    merely a final packaging step.
-   Commit after each logically complete, independently understandable
    milestone.
-   Make an early checkpoint once the first coherent part of the
    implementation is ready; do not accumulate the entire task into one
    enormous final commit.
-   Commit before risky follow-on work when doing so creates a useful
    recovery point.
-   Prefer several focused commits over one large commit, but do not
    create a commit for every tiny edit.
-   Use Conventional Commit-style messages, for example:
    -   `feat: add batch image processing`
    -   `fix: handle missing image inputs`
    -   `refactor: extract workflow transformation`
    -   `test: cover invalid image inputs`
    -   `docs: explain batch processing`
-   Before committing, inspect the staged diff and ensure it contains
    only the intended changes.
-   Never include unrelated user changes in a commit. Stage specific
    files or hunks rather than blindly staging everything when the
    worktree contains unrelated changes.
-   Do not amend existing commits or rewrite published history unless
    explicitly requested.

## 5. Build and test deliberately

-   Do not build, run tests, or run type checks after every individual
    edit.
-   Batch related changes and complete a coherent implementation
    milestone before deciding whether verification is useful.
-   During implementation, use code inspection and reasoning where
    sufficient; do not use repeated builds as a substitute for
    understanding the code.
-   Run targeted checks at meaningful milestones when they materially
    reduce risk, and perform appropriate final verification when the
    implementation is complete.
-   Do not repeatedly run the same failing command without a meaningful
    diagnostic or implementation change.
-   If a command fails, inspect the output, identify the likely cause,
    and change the diagnosis or approach before trying again.
-   Ask before running expensive builds, lengthy test suites,
    destructive integration tests, or commands that may affect external
    services or production data.
-   Never claim a build, test, lint, or type check passed unless it was
    actually run and passed.
-   If verification is skipped, say so clearly and explain any relevant
    risk.

## 6. Preserve user work and protect Git history

-   Treat pre-existing uncommitted changes as the user's work unless
    clearly established otherwise.
-   Never overwrite, discard, reset, stash, or incorporate pre-existing
    changes without understanding their purpose and obtaining permission
    when necessary.
-   Never run destructive Git commands such as `git reset --hard`,
    `git clean`, or force-push without explicit permission.
-   Do not delete branches, tags, commits, or untracked files unless
    explicitly requested.
-   Do not bypass repository hooks or verification safeguards with
    options such as `--no-verify` unless explicitly authorized.
-   If the working tree contains unexpected changes, stop and clarify
    before taking an action that could affect them.

## 7. Code quality and maintainability

-   Follow the project's existing language, formatting, architecture,
    and naming conventions.
-   Prefer clear, maintainable code over clever or unnecessarily compact
    implementations.
-   Keep functions, classes, and modules focused; avoid needless
    duplication.
-   Handle expected errors explicitly and provide useful diagnostics.
-   Do not silently swallow exceptions.
-   Consider realistic edge cases, invalid inputs, resource cleanup, and
    concurrency where relevant.
-   Avoid excessive defensive code for impossible or irrelevant
    scenarios.
-   Update relevant documentation and examples when user-facing
    behaviour, APIs, configuration, or operational procedures change.
-   Do not add temporary debugging code, dead code, or unused imports to
    the final implementation.

## 8. Troubleshooting

-   Diagnose failures from their actual error messages and surrounding
    code.
-   Avoid repeating an unsuccessful approach without new evidence or a
    meaningful change.
-   If an approach fails twice, pause and reassess the underlying
    assumption or choose a different diagnostic path.
-   Do not make speculative changes to unrelated components to see
    whether they help.
-   If blocked, explain what is known, what remains uncertain, and the
    most practical alternatives.

## 9. Autonomy and approvals

-   Make routine implementation decisions independently; do not ask
    permission for every minor choice.
-   Ask for clarification when requirements are materially ambiguous.
-   Seek approval before major architectural changes, introducing
    substantial dependencies, changing deployment or infrastructure
    behaviour, modifying secrets or credentials, or affecting production
    data or services.
-   Do not silently substitute a materially different solution when the
    requested approach is impractical; explain the trade-off first.
-   Explain important trade-offs briefly when multiple viable approaches
    exist.

## 10. Completion and handoff

Before reporting a substantial task complete:

-   Review the full diff, including staged and unstaged changes.
-   Check for accidental files, unrelated edits, debug output, and
    incomplete implementation.
-   Ensure intended milestones have been committed, where committing is
    appropriate.
-   Report the branch name and a concise list of commits created.
-   Summarize the implementation and any important design decisions.
-   List the verification commands actually run and their results.
-   Explicitly identify checks not run, known limitations, and remaining
    work.
-   Do not automatically merge or push the branch.

## 11. Working notes for long tasks

-   For work that spans many sessions or has several milestones,
    maintain a short task note in the repository (for example, `TODO.md`
    or a task-specific plan) only when it provides real value.
-   Keep notes current: completed milestones, decisions, outstanding
    work, and relevant commands.
-   Do not create planning or tracking files for small tasks that do not
    need them.

## Default workflow summary

For significant work, follow this sequence:

1.  Explore the repository and inspect Git status.
2.  Plan the change and split it into meaningful milestones.
3.  Create a feature branch.
4.  Implement a coherent milestone.
5.  Review and commit that milestone.
6.  Continue in focused increments, committing useful checkpoints.
7.  Verify at sensible milestones and once implementation is
    complete---not after every edit.
8.  Review the final diff and report commits, verification, and
    remaining issues.
