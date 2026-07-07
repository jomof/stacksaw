#!/usr/bin/env bash
# The rebase testbed. One repo, three staircases, exercising every rebase state:
#
#   step-1 -> step-2 -> step-3   behind main; step-1 adds a file, step-2/step-3
#                                rewrite Config.kt's PORT line that main also
#                                moved -> a *downstream* conflict on rebase, so
#                                the Stacks row shows the warn "rebase" glyph and
#                                Commits reads "rebase - will conflict".
#
#   hot-1 -> hot-2               behind main but only *adds* files main never
#                                touches -> a clean rebase, shown as the green
#                                "rebase" glyph / "rebase available".
#
#   amd-1 -> amd-2 -> amd-3      NOT behind main, but amd-1 was *amended* after
#                                amd-2/amd-3 were stacked on it, so its children
#                                dangle on its former tip. stacksaw recovers them
#                                via amd-1's reflog, reforms the family into one
#                                staircase, marks the amd-2 link stale, and (since
#                                amd-2 rewrites the same line amd-1 now owns)
#                                probes the *restack* as a conflict -> Stacks
#                                shows the warn glyph, Commits reads "restack -
#                                will conflict". This is the C fixture, ready to
#                                view with no manual rebase needed.
#
# Plus three flat single-branch twins with the same commit content, so a flat
# branch and a staircase can be compared side by side:
#
#   flat-step                    step's add + two PORT rewrites on one branch;
#                                behind main -> rebase conflict (identical verdict
#                                to the staircase, just not split into steps).
#   flat-hot                     hot's two pure adds on one branch; behind main
#                                -> rebase available.
#   flat-amd                     amd's commits on one branch, forked from current
#                                main and never amended -> not behind, no reflow.
#                                A restack needs a dangling child branch, so it
#                                has no single-branch analog; the twin just shows
#                                three commits ahead.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../lib.sh"

init_repo "rebase-conflict"

# config <port> <message> — (re)write Config.kt with the given PORT and commit
# on the current branch. Unquoted heredoc so $1 (the port) expands.
config() {
  commit_stdin "src/main/kotlin/demo/Config.kt" "$2" <<KT
package demo

/** Server configuration for the demo service. */
object Config {
    const val HOST: String = "127.0.0.1"
    const val PORT: Int = $1
}
KT
}

# Seed Config.kt on main so both the stack and main can move its PORT line.
config 8080 $'Add Config with default port 8080\n\nBaseline for the rebase testbed. Config.kt PORT starts at 8080; main\nlater moves this same line to 3000, which is what the step stack\ncollides with when it rebases.'

# The conflict stack: step-1 is an unrelated add; step-2/step-3 rewrite PORT.
new_branch "step-1" "main"; track "step-1" "main"
commit_stdin "src/main/kotlin/demo/Feature.kt" $'step-1: add feature flag\n\nAdds Feature.kt, a file main never touches. On rebase onto main this\nreplays cleanly \xe2\x80\x94 the conflict is downstream, at step-2.' <<'KT'
package demo

/** Feature flags for the demo service. */
object Feature {
    const val FAST_PATH: Boolean = true
}
KT
new_branch "step-2" "step-1"; track "step-2" "main"
config 9090 $'step-2: bump port to 9090\n\nRewrites Config.kt PORT 8080 -> 9090. main independently moved the same\nline to 3000, so this is the FIRST commit to conflict when step rebases\nonto main (3-way merge: base 8080, ours 9090, theirs 3000). The rebase\nhalts here.'
new_branch "step-3" "step-2"; track "step-3" "main"
config 9091 $'step-3: bump port to 9091\n\nRewrites PORT 9090 -> 9091, which would also conflict against main 3000.\nBut the rebase already stopped at step-2, so step-3 is never reached.'

# The clean stack: only adds files main never touches, so it rebases cleanly.
new_branch "hot-1" "main"; track "hot-1" "main"
commit_stdin "docs/NOTES.md" $'hot-1: add notes\n\nAdds docs/NOTES.md, a file main never touches, so it replays cleanly on\nrebase.' <<'MD'
# Notes

Operational notes for the demo service.
MD
new_branch "hot-2" "hot-1"; track "hot-2" "main"
commit_stdin "src/main/kotlin/demo/Api.kt" $'hot-2: add api stub\n\nAdds Api.kt, also untouched by main. With hot-1 a pure add too, the whole\nhot stack rebases onto main with no 3-way clash -> rebase available.' <<'KT'
package demo

interface Api {
    fun ping(): String
}
KT

# --- Flat single-branch twins -------------------------------------------------
# The same commit *content* as the staircases above, but each collapsed onto one
# branch (no ancestry splits), so you can compare how a flat branch and a
# staircase render and reflow. flat-step and flat-hot fork here (pre-move) so
# they end up behind main, mirroring step and hot.

# flat-step — step's commits on a single branch: an add, then two PORT rewrites.
new_branch "flat-step" "main"; track "flat-step" "main"
commit_stdin "src/main/kotlin/demo/Feature.kt" $'flat-step: add feature flag\n\nFlat twin of the step staircase, all on one branch. Adds Feature.kt (a\nclean add); the two PORT rewrites below are what conflict on rebase.' <<'KT'
package demo

/** Feature flags for the demo service. */
object Feature {
    const val FAST_PATH: Boolean = true
}
KT
config 9090 $'flat-step: bump port to 9090\n\nRewrites Config.kt PORT 8080 -> 9090. Being flat changes nothing about the\nconflict: replaying onto main (PORT 3000) still clashes here first (3-way\nmerge: base 8080, ours 9090, theirs 3000).'
config 9091 $'flat-step: bump port to 9091\n\nRewrites PORT 9090 -> 9091, which would also conflict, but the rebase\nhalts at the 9090 commit above.'

# flat-hot — hot's commits on a single branch: two pure adds.
new_branch "flat-hot" "main"; track "flat-hot" "main"
commit_stdin "docs/NOTES.md" $'flat-hot: add notes\n\nFlat twin of the hot staircase. Adds docs/NOTES.md, a file main never\ntouches.' <<'MD'
# Notes

Operational notes for the demo service.
MD
commit_stdin "src/main/kotlin/demo/Api.kt" $'flat-hot: add api stub\n\nAdds Api.kt (also untouched by main). Both commits are pure adds, so the\nflat branch rebases onto main clean -> rebase available.' <<'KT'
package demo

interface Api {
    fun ping(): String
}
KT

# Advance main past both forks with a conflicting change to the PORT line, so
# both stacks are now behind: `step` would conflict on rebase, `hot` would not.
gitr checkout -q main
config 3000 $'main: move port to 3000\n\nAdvances main past both the step and hot forks by moving the shared PORT\nline to 3000. This is the "theirs" side of the merge that makes step\nconflict, and it leaves both step and hot behind by one commit.'

# amd <value> <message> — (re)write Amd.kt with the given VALUE and commit on the
# current branch (mirrors config() but for the amend family's own file).
amd() {
  commit_stdin "src/main/kotlin/demo/Amd.kt" "$2" <<KT
package demo

/** Tunable owned by the amd family. */
object Amd {
    const val VALUE: Int = $1
}
KT
}

# The amend family: amd-1 owns Amd.kt, amd-2 rewrites the same VALUE line, amd-3
# only adds a file. Forked from the *current* main, so it is not behind — the
# restack is the sole signal.
new_branch "amd-1" "main"; track "amd-1" "main"
amd 1 $'amd-1: introduce Amd.VALUE\n\nIntroduces Amd.kt with VALUE=1; amd-2/amd-3 stack on top of this commit.\nIt is amended below (VALUE=9), which orphans those children onto this,\nits former tip.'
new_branch "amd-2" "amd-1"; track "amd-2" "main"
amd 2 $'amd-2: raise VALUE to 2\n\nRewrites Amd.kt VALUE 1 -> 2. After amd-1 is amended to VALUE=9,\nrestacking replays this commit onto the new amd-1 and conflicts on the\nsame line (3-way merge: base 1, ours 2, theirs 9). First conflict of the\nrestack; it halts here.'
new_branch "amd-3" "amd-2"; track "amd-3" "main"
commit_stdin "src/main/kotlin/demo/AmdExtra.kt" $'amd-3: add helper\n\nAdds AmdExtra.kt, a pure add that would restack cleanly. But the restack\nhalts at amd-2, so amd-3 is never reached.' <<'KT'
package demo

/** Extra helper stacked above the amd tunable. */
object AmdExtra {
    fun doubled(): Int = Amd.VALUE * 2
}
KT

# Amend amd-1 so amd-2/amd-3 dangle on its former tip. amd-1 now sets VALUE to a
# value that collides with amd-2's rewrite, so restacking amd-2 conflicts.
gitr checkout -q amd-1
cat > "$REPO/src/main/kotlin/demo/Amd.kt" <<'KT'
package demo

/** Tunable owned by the amd family. */
object Amd {
    const val VALUE: Int = 9
}
KT
gitr commit -q -a --amend -m $'amd-1: introduce Amd.VALUE (amended)\n\nThe amend. Rewrites VALUE 1 -> 9 in place, so amd-1 new tip diverges from\nthe old one amd-2/amd-3 were built on. Those children now dangle on the\nformer tip; stacksaw recovers them via reflog and flags a restack, which\nconflicts because VALUE=9 collides with amd-2 VALUE=2.'

# flat-amd — amd's commits on a single branch, forked from the *current* main
# and never amended. The point of this twin: a restack signal is staircase-only
# (it needs a dangling child branch), so the flat version simply shows three
# commits ahead with nothing to reflow.
new_branch "flat-amd" "main"; track "flat-amd" "main"
amd 1 $'flat-amd: introduce Amd.VALUE\n\nFlat twin of the amd family: the same commits, but on one branch with no\namend. Introduces Amd.kt VALUE=1.'
amd 2 $'flat-amd: raise VALUE to 2\n\nRewrites Amd.kt VALUE 1 -> 2. On a flat branch this is just ordinary\nhistory: there is no earlier commit being rewritten out from under it.'
commit_stdin "src/main/kotlin/demo/AmdExtra.kt" $'flat-amd: add helper\n\nAdds AmdExtra.kt. Forked from current main and never amended, so this\nbranch is not behind and needs no reflow -- the restack signal has no flat\nanalog.' <<'KT'
package demo

/** Extra helper stacked above the amd tunable. */
object AmdExtra {
    fun doubled(): Int = Amd.VALUE * 2
}
KT

# Leave HEAD on the conflict stack's tip so it opens first.
gitr checkout -q step-3

echo "built rebase-conflict at $REPO"
