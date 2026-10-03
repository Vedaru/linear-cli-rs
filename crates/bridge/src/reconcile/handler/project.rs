//! Putting an issue on a project's board, and keeping the board honest.
//!
//! Split out of `handler.rs` (VED-288); see `deliver` for the visibility rules this split uses.

use super::*;

impl ReconcileHandler {
    /// The project the other end names, resolved through the project pairing.
    ///
    /// The source names a project by its own id; the sink knows it by a different
    /// one. The pairing recorded when the projects were mirrored is what translates
    /// between them. A project the issue names that is not paired (or no project at
    /// all) resolves to `None` - which places nothing and is deliberately not an
    /// error, because a forge without that project has nothing to do rather than a
    /// reason to fail the whole issue.
    pub(super) fn project_on(
        &mut self,
        from: &Endpoint,
        onto: &ConnectorId,
        named: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(named) = named else {
            return Ok(None);
        };
        let reference = EntityRef {
            connector: from.connector.clone(),
            kind: EntityKind::Project,
            scope: Some(from.scope.clone()),
            native_id: named.to_string(),
            url: None,
        };
        let Some(link) = self.store.find_link(&reference, onto)? else {
            return Ok(None);
        };
        Ok(link
            .counterpart(&reference)
            .map(|found| found.native_id.clone()))
    }

    /// Put a newly mirrored issue on the board of the project it names, and record
    /// where it landed. A no-op for anything but an issue, and for an issue whose
    /// source named no paired project.
    pub(super) fn place_on_project(
        &mut self,
        pair: &Pair<'_>,
        issue: &EntityRef,
        project: Option<&str>,
        column: Option<&str>,
    ) -> Result<()> {
        if pair.subject.kind != EntityKind::Issue || !pair.project_mirroring {
            return Ok(());
        }
        if let Some(project) = project {
            self.sink(&pair.there.connector)?.place_issue(
                &pair.there.scope,
                &issue.native_id,
                project,
                column,
            )?;
            log::info!(
                "placed {} {} on project {}",
                pair.there.connector,
                issue.native_id,
                project
            );
        }
        // Recorded whether or not there was a project, so a pairing that never had
        // one is not later mistaken for one that did.
        self.store.set_link_project(pair.subject, project)?;
        Ok(())
    }

    /// Carry the container part of an issue update: a changed project moves the
    /// issue (a forge issue sits on one project, so assigning the new board takes it
    /// off the old), and a cleared one takes it off the board the record names.
    ///
    /// A board is also where a *state* lives as a column, so a state that moved places
    /// the card again in the column the mapping names for it. Only a board that was told
    /// what its columns mean moves anything: with no column named, the card stays exactly
    /// where a human put it.
    pub(super) fn settle_project(
        &mut self,
        pair: &Pair<'_>,
        issue: &EntityRef,
        patch: &Patch,
        fields: &IssueFields,
        column: Option<&str>,
        state_moving: bool,
    ) -> Result<()> {
        if pair.subject.kind != EntityKind::Issue || !pair.project_mirroring {
            return Ok(());
        }
        let desired = fields.project.as_deref();
        match &patch.project {
            Change::Set(_) => {
                if let Some(project) = desired {
                    self.sink(&pair.there.connector)?.place_issue(
                        &pair.there.scope,
                        &issue.native_id,
                        project,
                        column,
                    )?;
                    log::info!(
                        "moved {} {} to project {}",
                        pair.there.connector,
                        issue.native_id,
                        project
                    );
                }
            }
            Change::Clear => {
                // The id to remove it from is the one the pairing recorded: the
                // platform reports it nowhere, and the patch only says "cleared".
                let previous = self.store.link_project(pair.subject)?;
                if let Some(project) = previous.as_deref() {
                    self.sink(&pair.there.connector)?.remove_issue(
                        &pair.there.scope,
                        &issue.native_id,
                        project,
                    )?;
                    log::info!(
                        "removed {} {} from project {}",
                        pair.there.connector,
                        issue.native_id,
                        project
                    );
                }
            }
            Change::Leave => {
                // The container did not change, but the *state* may have - and on a board
                // the state is the column, so the card has to move with it. Onto the
                // project the pairing recorded, because a forge does not report which one
                // an issue is on and the patch says nothing about it.
                if state_moving {
                    if let Some(column) = column {
                        if let Some(project) = self.store.link_project(pair.subject)? {
                            self.sink(&pair.there.connector)?.place_issue(
                                &pair.there.scope,
                                &issue.native_id,
                                &project,
                                Some(column),
                            )?;
                            log::info!(
                                "moved {} {} to column `{}` of project {}",
                                pair.there.connector,
                                issue.native_id,
                                column,
                                project
                            );
                        }
                    }
                }
            }
        }
        self.store.set_link_project(pair.subject, desired)?;
        Ok(())
    }
}
