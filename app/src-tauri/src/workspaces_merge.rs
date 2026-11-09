//! Three-way merge for `workspaces.json`.
//!
//! `persist()` writes the WHOLE in-memory struct, which is
//! last-write-wins over the entire document. That is fine while one app
//! owns the file and wrong the moment two do — and two do, routinely: a
//! stable ymux and a dev build share `%APPDATA%\ymux` unless someone
//! remembers `YMUX_CONFIG_DIR`. The pre-rename `%APPDATA%\winmux` and
//! `WINMUX_CONFIG_DIR` are still honoured, so an old build and a new one
//! can land on the same directory from either side of the rename.
//!
//! So a save is no longer "dump what I have". It re-reads the file, and
//! if it changed since we last touched it, applies only OUR delta on top
//! of THEIRS.
//!
//! The merge is at the JSON level rather than over `WorkspacesFile`, for
//! one reason that matters: it then works for fields this binary has
//! never heard of. An older build merging a file written by a newer one
//! carries the newer keys through untouched, which is the entire failure
//! this module exists to stop.
//!
//! ## The rule that is not obvious
//!
//! A field that vanished on their side is NOT treated as a deletion when
//! we still hold a value for it. That looks wrong until you see the
//! failure it prevents: an older binary does not "delete" `parent_id`, it
//! simply never writes it, and a naive merge reads that absence as
//! intent and honours it — silently destroying the nesting. A writer that
//! genuinely understood the field and wanted it gone would have had to
//! know the field existed. Between "undo a rare deliberate clear" and
//! "lose data every time an old build saves", the first is the cheaper
//! mistake, and it is logged.

use serde_json::{Map, Value};

/// Merge `ours` onto `theirs`, using `base` — the content as we last read
/// or wrote it — to tell an edit apart from an absence.
///
/// Returns the merged document plus one line per non-obvious decision, so
/// a save that quietly rescued a field says so in the log.
pub(crate) fn merge_workspaces(
    base: &Value,
    ours: &Value,
    theirs: &Value,
) -> (Value, Vec<String>) {
    let mut notes = Vec::new();
    let merged = merge_object(base, ours, theirs, "", &mut notes);
    (merged, notes)
}

/// Workspaces and groups are arrays keyed by `id`, not positional lists.
/// Merging them elementwise by index would pair unrelated rows the moment
/// one side reorders.
fn is_keyed_array(path: &str) -> bool {
    matches!(path, "/workspaces" | "/groups" | "/project_folders")
}

fn id_of(v: &Value) -> Option<&str> {
    v.get("id").and_then(Value::as_str)
}

fn merge_object(base: &Value, ours: &Value, theirs: &Value, path: &str, notes: &mut Vec<String>) -> Value {
    match (ours, theirs) {
        (Value::Object(o), Value::Object(t)) => {
            let empty = Map::new();
            let b = base.as_object().unwrap_or(&empty);
            let mut out = Map::new();

            // Every key either side knows about.
            let mut keys: Vec<&String> = t.keys().chain(o.keys()).collect();
            keys.sort();
            keys.dedup();

            for k in keys {
                let bv = b.get(k);
                let ov = o.get(k);
                let tv = t.get(k);
                let child = format!("{path}/{k}");
                match (ov, tv) {
                    (Some(ov), Some(tv)) => {
                        if is_keyed_array(&child) {
                            out.insert(k.clone(), merge_keyed_array(bv, ov, tv, &child, notes));
                        } else if ov.is_object() && tv.is_object() {
                            out.insert(
                                k.clone(),
                                merge_object(bv.unwrap_or(&Value::Null), ov, tv, &child, notes),
                            );
                        } else if Some(ov) != bv {
                            // We changed it; our value wins.
                            out.insert(k.clone(), ov.clone());
                        } else {
                            // We did not touch it — theirs is newer.
                            out.insert(k.clone(), tv.clone());
                        }
                    }
                    // Only we have it. Either we just added it, or they
                    // dropped a key they do not understand. Both cases
                    // keep ours; only the second is worth a line.
                    (Some(ov), None) => {
                        if bv.is_some() {
                            notes.push(format!("kept {child} — absent in the file on disk"));
                        }
                        out.insert(k.clone(), ov.clone());
                    }
                    // Only they have it: their addition, or a key WE do
                    // not understand. Carry it through untouched.
                    (None, Some(tv)) => {
                        out.insert(k.clone(), tv.clone());
                    }
                    (None, None) => unreachable!("key came from one of the two maps"),
                }
            }
            Value::Object(out)
        }
        // Not both objects — fall back to "ours if we edited it".
        _ => {
            if ours != base {
                ours.clone()
            } else {
                theirs.clone()
            }
        }
    }
}

fn merge_keyed_array(
    base: Option<&Value>,
    ours: &Value,
    theirs: &Value,
    path: &str,
    notes: &mut Vec<String>,
) -> Value {
    let empty: Vec<Value> = Vec::new();
    let ov = ours.as_array().unwrap_or(&empty);
    let tv = theirs.as_array().unwrap_or(&empty);
    let bv = base.and_then(Value::as_array).unwrap_or(&empty);

    let find = |arr: &'_ [Value], id: &str| arr.iter().find(|e| id_of(e) == Some(id)).cloned();

    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    // Their order is the base order — they wrote most recently.
    for t in tv {
        let Some(id) = id_of(t).map(str::to_string) else {
            out.push(t.clone());
            continue;
        };
        seen.push(id.clone());
        match find(ov, &id) {
            Some(o) => {
                let b = find(bv, &id).unwrap_or(Value::Null);
                out.push(merge_object(&b, &o, t, &format!("{path}/*"), notes));
            }
            // We removed it and they still have it: honour our deletion
            // only if we actually had it to begin with.
            None => {
                if find(bv, &id).is_none() {
                    out.push(t.clone());
                } else {
                    notes.push(format!("{path}: dropped {id} — deleted on our side"));
                }
            }
        }
    }

    // Anything we have that they do not: our additions, or rows an older
    // build never wrote back.
    for o in ov {
        let Some(id) = id_of(o).map(str::to_string) else { continue };
        if seen.contains(&id) {
            continue;
        }
        if find(bv, &id).is_some() {
            notes.push(format!("{path}: restored {id} — missing from the file on disk"));
        }
        out.push(o.clone());
    }

    Value::Array(out)
}


/// Decide what bytes actually go to disk.
///
/// `ours` is what this process wants to write, `base` is the file as we
/// last read or wrote it (None on the very first save of a run), and
/// `on_disk` is what is there right now.
///
/// Returns the text to write plus any lines worth logging. Splitting this
/// out of `save_to_disk` is not tidiness: an idle ymux never saves, so
/// the merge path is unreachable by launching the app and waiting, and
/// the only way to hold it to account is to call it directly.
pub(crate) fn reconcile(
    ours: &str,
    base: Option<&str>,
    on_disk: &str,
) -> (String, Vec<String>) {
    let disk = on_disk.strip_prefix('\u{feff}').unwrap_or(on_disk);
    let Some(base) = base else {
        // First save of the run: nothing to compare against.
        return (ours.to_string(), Vec::new());
    };
    if disk.trim().is_empty() || disk == base {
        // Nobody else wrote — the overwhelmingly common case.
        return (ours.to_string(), Vec::new());
    }
    match (
        serde_json::from_str::<Value>(base),
        serde_json::from_str::<Value>(ours),
        serde_json::from_str::<Value>(disk),
    ) {
        (Ok(b), Ok(o), Ok(t)) => {
            let (mut merged, mut notes) = merge_workspaces(&b, &o, &t);
            // Our version always wins. `ours` equals `base` for this scalar,
            // so the merge rule would keep an older build's number on disk
            // for as long as we keep saving. The gate already refused a newer one.
            if let (Some(m), Some(v)) = (merged.as_object_mut(), o.get("version")) {
                m.insert("version".to_string(), v.clone());
            }
            let text = match serde_json::to_string_pretty(&merged) {
                Ok(t) => t,
                // Merged fine but will not serialize: writing `ours` is
                // the old behaviour and better than losing the save.
                Err(e) => {
                    notes.push(format!("merge result would not serialize ({e}) — wrote our own state"));
                    return (ours.to_string(), notes);
                }
            };
            notes.insert(
                0,
                format!(
                    "the file changed under us ({} bytes on disk vs {} known) — merged instead of overwriting",
                    disk.len(),
                    base.len()
                ),
            );
            (text, notes)
        }
        // Unparseable on some side: keep the old behaviour rather than
        // invent one, but say so out loud.
        _ => (
            ours.to_string(),
            vec![
                "the file changed under us but could not be parsed for a merge — wrote our own state"
                    .to_string(),
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ws(id: &str, extra: Value) -> Value {
        let mut v = json!({ "id": id, "name": id });
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            for (k, val) in e {
                o.insert(k.clone(), val.clone());
            }
        }
        v
    }


    // ── reconcile: the seam save_to_disk actually calls ────────────────

    const OURS: &str = r#"{"version":1,"workspaces":[{"id":"club","name":"club","parent_id":"runner1","is_project_root":true}]}"#;
    const STRIPPED: &str = r#"{"version":1,"workspaces":[{"id":"club","name":"club"}]}"#;

    #[test]
    fn reconcile_rescues_a_field_an_older_build_wrote_away() {
        // End to end at the seam: we hold the nesting, the file on disk
        // lost it while we were running. Writing `ours` blindly would be
        // right here by luck — so make the merge prove itself instead by
        // giving the other side a change we must NOT clobber.
        let base = OURS;
        let ours = OURS;
        let disk = r#"{"version":1,"workspaces":[{"id":"club","name":"renamed by the other app"}]}"#;

        let (text, notes) = reconcile(ours, Some(base), disk);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["workspaces"][0]["parent_id"], "runner1", "our nesting survived");
        assert_eq!(
            v["workspaces"][0]["name"], "renamed by the other app",
            "and their edit survived too"
        );
        assert!(notes.iter().any(|n| n.contains("merged instead of overwriting")), "{notes:?}");
    }

    #[test]
    fn a_merge_writes_our_schema_version_over_an_older_one() {
        // Pins the version after a downgrade: a stuck older number lets an
        // older build pass its own schema gate on the next write.
        let ours = r#"{"version":4,"workspaces":[]}"#;
        let disk = r#"{"version":1,"workspaces":[]}"#;
        let (text, _) = reconcile(ours, Some(ours), disk);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["version"], 4);
    }

    #[test]
    fn reconcile_is_a_no_op_when_nothing_else_touched_the_file() {
        let (text, notes) = reconcile(OURS, Some(OURS), OURS);
        assert_eq!(text, OURS);
        assert!(notes.is_empty(), "the common path must stay silent: {notes:?}");
    }

    #[test]
    fn reconcile_writes_ours_on_the_first_save_of_a_run() {
        let (text, notes) = reconcile(OURS, None, STRIPPED);
        assert_eq!(text, OURS);
        assert!(notes.is_empty());
    }

    #[test]
    fn reconcile_survives_an_unparseable_file_on_disk() {
        let (text, notes) = reconcile(OURS, Some(OURS), "{ this is not json");
        assert_eq!(text, OURS, "a corrupt file must not block the save");
        assert!(notes.iter().any(|n| n.contains("could not be parsed")), "{notes:?}");
    }

    #[test]
    fn reconcile_ignores_a_bom_the_other_writer_left_behind() {
        let bom_disk = format!("\u{feff}{STRIPPED}");
        let (text, _) = reconcile(OURS, Some(STRIPPED), &bom_disk);
        // Same content once the mark is off, so this is the no-op path.
        assert_eq!(text, OURS);
    }

    #[test]
    fn an_older_build_dropping_a_field_it_never_knew_does_not_delete_it() {
        // The incident, exactly: both apps load a file where `club` is
        // nested. The old build rewrites it without parent_id /
        // is_project_root, because its Workspace struct has no such
        // fields. Our next save must NOT adopt that as a deletion.
        let base = json!({ "version": 1, "workspaces": [
            ws("club", json!({ "parent_id": "runner1", "is_project_root": true }))
        ]});
        let ours = base.clone();
        let theirs = json!({ "version": 1, "workspaces": [ ws("club", json!({})) ]});

        let (merged, notes) = merge_workspaces(&base, &ours, &theirs);
        let club = &merged["workspaces"][0];
        assert_eq!(club["parent_id"], "runner1", "nesting must survive");
        assert_eq!(club["is_project_root"], true);
        assert!(
            notes.iter().any(|n| n.contains("parent_id")),
            "the rescue has to be visible in the log, got {notes:?}"
        );
    }

    #[test]
    fn a_field_the_other_side_actually_changed_wins_when_we_did_not_touch_it() {
        let base = json!({ "workspaces": [ ws("w1", json!({ "name": "old" })) ]});
        let ours = base.clone();
        let mut theirs = base.clone();
        theirs["workspaces"][0]["name"] = json!("renamed elsewhere");

        let (merged, _) = merge_workspaces(&base, &ours, &theirs);
        assert_eq!(merged["workspaces"][0]["name"], "renamed elsewhere");
    }

    #[test]
    fn our_edit_wins_over_their_stale_value() {
        let base = json!({ "workspaces": [ ws("w1", json!({ "name": "old" })) ]});
        let mut ours = base.clone();
        ours["workspaces"][0]["name"] = json!("ours");
        let theirs = base.clone();

        let (merged, _) = merge_workspaces(&base, &ours, &theirs);
        assert_eq!(merged["workspaces"][0]["name"], "ours");
    }

    #[test]
    fn both_sides_keep_their_own_new_workspaces() {
        let base = json!({ "workspaces": [ ws("shared", json!({})) ]});
        let mut ours = base.clone();
        ours["workspaces"].as_array_mut().unwrap().push(ws("mine", json!({})));
        let mut theirs = base.clone();
        theirs["workspaces"].as_array_mut().unwrap().push(ws("theirs", json!({})));

        let (merged, _) = merge_workspaces(&base, &ours, &theirs);
        let ids: Vec<&str> = merged["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(id_of)
            .collect();
        assert!(ids.contains(&"shared") && ids.contains(&"mine") && ids.contains(&"theirs"), "{ids:?}");
    }

    #[test]
    fn a_deletion_we_made_is_honoured_and_one_we_never_had_is_not() {
        // We deleted `gone`; they still list it. We never had `new`.
        let base = json!({ "workspaces": [ ws("keep", json!({})), ws("gone", json!({})) ]});
        let ours = json!({ "workspaces": [ ws("keep", json!({})) ]});
        let theirs = json!({ "workspaces": [
            ws("keep", json!({})), ws("gone", json!({})), ws("new", json!({}))
        ]});

        let (merged, _) = merge_workspaces(&base, &ours, &theirs);
        let ids: Vec<&str> = merged["workspaces"].as_array().unwrap().iter().filter_map(id_of).collect();
        assert_eq!(ids, vec!["keep", "new"], "our delete sticks, their add arrives");
    }

    #[test]
    fn keys_this_binary_has_never_heard_of_survive_untouched() {
        // The mirror image: WE are the old build. A newer one wrote a
        // field we cannot parse; merging must carry it through.
        let base = json!({ "workspaces": [ ws("w1", json!({})) ]});
        let ours = base.clone();
        let theirs = json!({ "workspaces": [ ws("w1", json!({ "from_the_future": 42 })) ]});

        let (merged, _) = merge_workspaces(&base, &ours, &theirs);
        assert_eq!(merged["workspaces"][0]["from_the_future"], 42);
    }

    #[test]
    fn reordering_on_one_side_does_not_pair_unrelated_rows() {
        let base = json!({ "workspaces": [ ws("a", json!({})), ws("b", json!({})) ]});
        let mut ours = base.clone();
        ours["workspaces"][0]["name"] = json!("a-renamed");
        let theirs = json!({ "workspaces": [ ws("b", json!({})), ws("a", json!({})) ]});

        let (merged, _) = merge_workspaces(&base, &ours, &theirs);
        let arr = merged["workspaces"].as_array().unwrap();
        let a = arr.iter().find(|e| id_of(e) == Some("a")).unwrap();
        assert_eq!(a["name"], "a-renamed", "the rename followed the id, not the index");
    }
}
