---- MODULE StoreCommit ----
\* One ready frame, matching store.rs, not a second implementation of it.
\* write_jpeg writes tmp/<id>.jpg.partial, syncs, then renames onto the
\* session frame path. commit inserts the row after that rename, and deletes
\* the renamed JPEG if the insert fails. open marks a session with a null
\* ended_at interrupted, wipes tmp/, and delete_orphan_jpegs removes a final
\* JPEG that no frame row points at. open is safe to run twice for that cleanup.
\* Symlinks, WAL, and a second frame are not in this model.

CONSTANT startupDeletesOrphan

VARIABLES partial, final, row, insertFailed, life, startupDue, crashKind, interrupted

vars == <<partial, final, row, insertFailed, life, startupDue, crashKind, interrupted>>

TypeOK ==
    /\ partial \in BOOLEAN
    /\ final \in BOOLEAN
    /\ row \in BOOLEAN
    /\ insertFailed \in BOOLEAN
    /\ life \in {"running", "crashed"}
    /\ startupDue \in BOOLEAN
    /\ crashKind \in {"none", "between_rename_and_insert", "other"}
    /\ interrupted \in BOOLEAN
    /\ ~(partial /\ final)
    /\ startupDeletesOrphan \in BOOLEAN

Init ==
    /\ partial = FALSE
    /\ final = FALSE
    /\ row = FALSE
    /\ insertFailed = FALSE
    /\ life = "running"
    /\ startupDue = FALSE
    /\ crashKind = "none"
    /\ interrupted = FALSE

WritePartial ==
    /\ life = "running"
    /\ partial = FALSE
    /\ final = FALSE
    /\ row = FALSE
    /\ insertFailed = FALSE
    /\ partial' = TRUE
    /\ UNCHANGED <<final, row, insertFailed, life, startupDue, crashKind, interrupted>>

Rename ==
    /\ life = "running"
    /\ partial = TRUE
    /\ final = FALSE
    /\ partial' = FALSE
    /\ final' = TRUE
    /\ UNCHANGED <<row, insertFailed, life, startupDue, crashKind, interrupted>>

Insert ==
    /\ life = "running"
    /\ final = TRUE
    /\ row = FALSE
    /\ insertFailed = FALSE
    /\ crashKind = "none"
    /\ startupDue = FALSE
    /\ row' = TRUE
    /\ UNCHANGED <<partial, final, insertFailed, life, startupDue, crashKind, interrupted>>

InsertFails ==
    /\ life = "running"
    /\ final = TRUE
    /\ row = FALSE
    /\ insertFailed = FALSE
    /\ crashKind = "none"
    /\ startupDue = FALSE
    /\ insertFailed' = TRUE
    /\ UNCHANGED <<partial, final, row, life, startupDue, crashKind, interrupted>>

DeleteRenamed ==
    /\ life = "running"
    /\ insertFailed = TRUE
    /\ final = TRUE
    /\ row = FALSE
    /\ final' = FALSE
    /\ UNCHANGED <<partial, row, insertFailed, life, startupDue, crashKind, interrupted>>

Crash ==
    /\ life = "running"
    /\ life' = "crashed"
    /\ startupDue' = TRUE
    /\ crashKind' = IF final /\ ~row /\ ~insertFailed
                    THEN "between_rename_and_insert"
                    ELSE "other"
    /\ UNCHANGED <<partial, final, row, insertFailed, interrupted>>

Startup ==
    /\ \/ life = "crashed"
       \/ (life = "running" /\ partial = FALSE /\ (final => row) /\ insertFailed = FALSE)
    /\ partial' = FALSE
    /\ IF startupDeletesOrphan /\ final /\ ~row THEN final' = FALSE ELSE final' = final
    /\ row' = row
    /\ life' = "running"
    /\ startupDue' = FALSE
    /\ interrupted' = TRUE
    /\ UNCHANGED <<insertFailed, crashKind>>

Next ==
    \/ WritePartial
    \/ Rename
    \/ Insert
    \/ InsertFails
    \/ DeleteRenamed
    \/ Crash
    \/ Startup

Spec == Init /\ [][Next]_vars

liveGap ==
    /\ life = "running"
    /\ crashKind = "none"
    /\ final
    /\ ~row

\* Prompt sentence, checked only by StoreCommitLiteral.cfg.
\* It does not hold in the live gap after Rename and before Insert, and it
\* does not hold after a crash between InsertFails and DeleteRenamed.
\* Those failures are expected. Do not add the live gap here to make this pass.
LiteralFinalJpegSentence ==
    final => row \/ (crashKind = "between_rename_and_insert" /\ startupDue)

\* After startup has returned, and outside the live gap, a final JPEG remains
\* only when a row points at it. This is the passing check of the contract.
\* A startup that keeps the orphan violates it.
NoOrphanFinalJpegAfterStartup ==
    (life = "running" /\ ~startupDue /\ ~liveGap) => (final => row)

====
