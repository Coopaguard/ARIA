//! The validations of hooks, kept in `aria.db`. See [`crate::hooks`].

use std::collections::HashMap;

use rusqlite::params;

use super::History;
use crate::hooks::Approval;

impl History {
    pub fn hook_approvals(&self) -> rusqlite::Result<HashMap<String, Approval>> {
        let mut statement = self
            .db
            .prepare("SELECT id, fingerprint, enabled FROM hook_approvals")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Approval {
                    fingerprint: row.get(1)?,
                    enabled: row.get::<_, i64>(2)? != 0,
                },
            ))
        })?;
        rows.collect()
    }

    /// Write them all: there are a handful, and one statement per change
    /// would be one more place to forget a hook.
    pub fn save_hook_approvals(&self, all: &HashMap<String, Approval>) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM hook_approvals", [])?;
        for (id, approval) in all {
            self.db.execute(
                "INSERT INTO hook_approvals(id, fingerprint, enabled) VALUES (?1, ?2, ?3)",
                params![id, approval.fingerprint, approval.enabled as i64],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validations_survive_a_restart_and_a_deleted_hook_goes() {
        let history = History::in_memory().unwrap();
        assert!(history.hook_approvals().unwrap().is_empty());
        let mut all = HashMap::from([
            ("mails".to_string(), Approval { fingerprint: "ab".into(), enabled: true }),
            ("maj".to_string(), Approval { fingerprint: "cd".into(), enabled: false }),
        ]);
        history.save_hook_approvals(&all).unwrap();
        assert_eq!(history.hook_approvals().unwrap(), all);
        all.remove("maj");
        history.save_hook_approvals(&all).unwrap();
        assert_eq!(history.hook_approvals().unwrap(), all);
    }
}
