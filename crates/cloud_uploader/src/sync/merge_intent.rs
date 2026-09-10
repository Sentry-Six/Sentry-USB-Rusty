use std::collections::BTreeSet;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sentryusb_drives::mutable_intent::{Edit, Intent};

fn tags(value: &Value) -> Result<Vec<String>> {
    value.as_array().context("unreadable Cloud tags")?.iter()
        .map(|value| value.as_str().map(String::from).context("unreadable Cloud tag")).collect()
}

pub(super) fn merge_tags(intent: &Intent, cloud: &Value) -> Result<Value> {
    ensure!(!intent.legacy, "older queued edit needs reconciliation");
    let mut result: BTreeSet<String> = tags(cloud)?.into_iter().collect();
    for (_, edit) in &intent.edits {
        if let Edit::Tags { added, removed } = edit {
            for tag in removed { result.remove(tag); }
            result.extend(added.iter().cloned());
        }
    }
    Ok(serde_json::to_value(result)?)
}

fn cost_core(value: &Value) -> Result<Option<(f64, &str)>> {
    if value.is_null() { return Ok(None) }
    let amount = value.get("amount").and_then(Value::as_f64).context("unreadable Cloud cost")?;
    let currency = value.get("currency").and_then(Value::as_str).context("unreadable Cloud cost currency")?;
    ensure!(amount.is_finite() && amount >= 0.0, "invalid Cloud cost");
    Ok(Some((amount,currency)))
}

pub(super) fn merge_charge(intent: &Intent, cloud: &Value) -> Result<Value> {
    ensure!(!intent.legacy, "older queued edit needs reconciliation");
    let mut result = cloud.as_object().context("unreadable Cloud charge mutable")?.clone();
    let current_tags = result.get("tags").context("missing Cloud tags")?;
    tags(current_tags)?;
    let current_cost = result.get("costOverride").unwrap_or(&Value::Null);
    let current_core = cost_core(current_cost)?;
    let costs: Vec<_> = intent.edits.iter().filter_map(|(_, edit)| match edit {
        Edit::CostOverride {before, after} => Some((before,after)), _ => None,
    }).collect();
    let next_cost = if let Some((_, desired)) = costs.last() {
        let mut matches_known = current_core == cost_core(costs[0].0)?;
        // An earlier request may have committed before its acknowledgement.
        for (_, after) in &costs { matches_known |= current_core == cost_core(after)?; }
        ensure!(matches_known, "charge cost changed in Cloud");
        cost_core(desired)?;
        if desired.is_null() { Some(Value::Null) } else {
            let mut value = current_cost.as_object().cloned().unwrap_or_default();
            value.insert("amount".into(), desired["amount"].clone());
            value.insert("currency".into(), desired["currency"].clone());
            Some(Value::Object(value))
        }
    } else { None };
    if intent.edits.iter().any(|(_, edit)| matches!(edit, Edit::Tags {..})) {
        result.insert("tags".into(), merge_tags(intent, current_tags)?);
    }
    if let Some(cost) = next_cost { result.insert("costOverride".into(), cost); }
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sentryusb_drives::{DriveStore, mutable_intent};

    fn intent(store: &DriveStore) -> Intent {
        store.with_locked_conn(|conn| mutable_intent::read(conn,"charge","1")).unwrap()
    }

    #[test]
    fn tag_edit_preserves_cloud_cost_and_unknown_fields() {
        let store=DriveStore::open_memory().unwrap();
        store.set_charge_tags_from_sync(1,&["Before".into()]).unwrap();
        store.set_charge_cost_from_sync(1,Some((4.0,"CAD".into()))).unwrap();
        store.set_charge_tags(1,&["Before".into(),"Work".into()]).unwrap();
        let cloud=json!({"tags":["Before","Remote"],"costOverride":{"amount":6.0,"currency":"CAD","receipt":"keep"},"extension":{"keep":true}});
        let merged=merge_charge(&intent(&store),&cloud).unwrap();
        assert_eq!(merged["tags"],json!(["Before","Remote","Work"]));
        assert_eq!(merged["costOverride"],cloud["costOverride"]);
        assert_eq!(merged["extension"],cloud["extension"]);
    }

    #[test]
    fn cost_edit_keeps_remote_tags_and_receipt_and_rejects_a_changed_cost() {
        let store=DriveStore::open_memory().unwrap();
        store.set_charge_cost_from_sync(1,Some((4.0,"CAD".into()))).unwrap();
        store.set_charge_cost(1,Some((8.0,"CAD".into()))).unwrap();
        let cloud=json!({"tags":["Remote"],"costOverride":{"amount":4.0,"currency":"CAD","receipt":"keep"}});
        let merged=merge_charge(&intent(&store),&cloud).unwrap();
        assert_eq!(merged["tags"],cloud["tags"]);assert_eq!(merged["costOverride"]["receipt"],"keep");
        assert_eq!(merged["costOverride"]["amount"],8.0);
        assert!(merge_charge(&intent(&store),&json!({"tags":[],"costOverride":{"amount":6.0,"currency":"CAD"}})).is_err());
    }

    #[test]
    fn acknowledged_tag_prefix_cannot_reintroduce_a_later_cloud_removal() {
        let store=DriveStore::open_memory().unwrap();store.set_charge_tags(1,&["X".into()]).unwrap();
        let first=store.dirty_mutables().unwrap()[0].2;
        store.set_charge_tags(1,&["X".into(),"Y".into()]).unwrap();
        store.clear_mutable_dirty("charge","1",first).unwrap();
        let pending=intent(&store);assert_eq!(pending.edits.len(),1);
        assert_eq!(merge_charge(&pending,&json!({"tags":["Remote"]})).unwrap()["tags"],json!(["Remote","Y"]));
    }

    #[test]
    fn undo_during_inflight_add_is_retained_after_old_acknowledgement() {
        let store=DriveStore::open_memory().unwrap();store.set_charge_tags(1,&["X".into()]).unwrap();
        let first=store.dirty_mutables().unwrap()[0].2;
        store.set_charge_tags(1,&[]).unwrap();store.clear_mutable_dirty("charge","1",first).unwrap();
        assert_eq!(merge_charge(&intent(&store),&json!({"tags":["X","Remote"]})).unwrap()["tags"],json!(["Remote"]));
    }

    #[test]
    fn cost_chain_can_resume_from_its_already_applied_prefix() {
        let store=DriveStore::open_memory().unwrap();store.set_charge_cost_from_sync(1,Some((4.0,"CAD".into()))).unwrap();
        store.set_charge_cost(1,Some((8.0,"CAD".into()))).unwrap();let first=store.dirty_mutables().unwrap()[0].2;
        store.set_charge_cost(1,Some((10.0,"CAD".into()))).unwrap();
        let cloud=json!({"tags":[],"costOverride":{"amount":8.0,"currency":"CAD"}});
        assert_eq!(merge_charge(&intent(&store),&cloud).unwrap()["costOverride"]["amount"],10.0);
        store.clear_mutable_dirty("charge","1",first).unwrap();
        assert_eq!(merge_charge(&intent(&store),&cloud).unwrap()["costOverride"]["amount"],10.0);
    }
}
