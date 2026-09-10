//! Merge recorded rate-field edits without replacing unrelated Cloud settings.
//! Objects merge by field; ordered schedule arrays and deleted plans are atomic.
use std::collections::BTreeSet;
use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value, json};
use crate::mutable_intent::{Edit, Intent, RATE_KEYS};

type Path = Vec<String>;
struct Event { before: Value, after: Value, paths: Vec<Path>, force: bool, required_tag:Option<String> }

fn number(value: Option<&Value>) -> Option<Option<f64>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(text)) if text.trim().is_empty() => Some(None),
        Some(value) => {
            let parsed = value.as_f64().or_else(|| value.as_str()?.trim().parse().ok())?;
            (parsed.is_finite() && parsed >= 0.0).then_some(Some(parsed))
        }
    }
}
fn plan(value: &Value) -> Result<Map<String, Value>> {
    if let Some(map) = value.as_object() { return Ok(map.clone()); }
    ensure!(number(Some(value)).is_some(), "unsupported rate plan");
    Ok(Map::from_iter([("flat".into(), value.clone())]))
}
fn plans(value: Option<&Value>) -> Result<Map<String, Value>> {
    match value {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map.clone()),
        Some(Value::String(text)) => serde_json::from_str::<Map<String, Value>>(text).context("unreadable rate plans"),
        _ => anyhow::bail!("unreadable rate plans"),
    }
}
fn view(document: &Value) -> Result<Value> {
    let mut result = document.as_object().context("rate document is not an object")?.clone();
    let normalized = plans(result.get("charging_tag_rates"))?.into_iter()
        .map(|(tag, value)| Ok((tag, Value::Object(plan(&value)?))))
        .collect::<Result<Map<_, _>>>()?;
    result.insert("charging_tag_rates".into(), Value::Object(normalized));
    Ok(Value::Object(result))
}
fn get<'a>(document: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(document, |value, key| value.as_object()?.get(key))
}
fn empty_schedules(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null() && !value.as_array().is_some_and(Vec::is_empty))
}
fn equal(path: &[String], a: Option<&Value>, b: Option<&Value>) -> bool {
    if a == b { return true; }
    if path == ["charging_default_rate"] || (path.len() == 3 && path[0] == "charging_tag_rates" && path[2] == "flat") {
        return matches!((number(a), number(b)), (Some(left), Some(right)) if left == right);
    }
    if path == ["charging_currency"] {
        let text = |value: Option<&Value>| match value {
            None | Some(Value::Null) => Some("$".to_string()),
            Some(Value::String(value)) => Some(if value.trim().is_empty() { "$" } else { value.trim() }.to_string()),
            _ => None,
        };
        return matches!((text(a), text(b)), (Some(left), Some(right)) if left == right);
    }
    if path.len() == 3 && path[0] == "charging_tag_rates" && path[2] == "schedules" {
        return empty_schedules(a) == empty_schedules(b);
    }
    if path.len() == 2 && path[0] == "charging_tag_rates" {
        if let (Some(Value::Object(left)), Some(Value::Object(right))) = (a, b) {
            let keys: BTreeSet<_> = left.keys().chain(right.keys()).collect();
            return keys.into_iter().all(|key| {
                let mut child = path.to_vec(); child.push(key.clone());
                equal(&child, left.get(key), right.get(key))
            });
        }
    }
    false
}
fn diff(path: Path, before: Option<&Value>, after: Option<&Value>, output: &mut Vec<Path>) {
    if equal(&path, before, after) { return; }
    if let (Some(Value::Object(before)), Some(Value::Object(after))) = (before, after) {
        let keys: BTreeSet<_> = before.keys().chain(after.keys()).collect();
        for key in keys {
            let mut child = path.clone(); child.push(key.clone());
            diff(child, before.get(key), after.get(key), output);
        }
    } else { output.push(path); }
}
fn events(intent: &Intent, after: Option<i64>) -> Result<Vec<Event>> {
    let mut result = Vec::new();
    for (at, edit) in &intent.edits {
        if after.is_some_and(|through| *at <= through) { continue; }
        let Edit::RateConfig { before, after, force, required_tag } = edit else { anyhow::bail!("unsupported rate intent"); };
        let before = view(before)?; let after = view(after)?; let mut paths = Vec::new();
        for key in RATE_KEYS { diff(vec![(*key).into()], before.get(*key), after.get(*key), &mut paths); }
        result.push(Event { before, after, paths, force: *force, required_tag:required_tag.clone() });
    }
    Ok(result)
}
fn set(document: &mut Value, path: &[String], value: Option<Value>) -> Result<()> {
    ensure!(!path.is_empty(), "empty rate field path");
    let root = document.as_object_mut().context("invalid rate document")?;
    if path[0] == "charging_tag_rates" && path.len() > 1 {
        let values = plans(root.get("charging_tag_rates"))?;
        root.insert("charging_tag_rates".into(), Value::Object(values));
        if path.len() > 2 {
            let values = root.get_mut("charging_tag_rates").unwrap().as_object_mut().unwrap();
            let value = values.get(&path[1]).map(plan).transpose()?.unwrap_or_default();
            values.insert(path[1].clone(), Value::Object(value));
        }
    }
    let mut cursor = root;
    for key in &path[..path.len() - 1] {
        let next = cursor.entry(key.clone()).or_insert_with(|| json!({}));
        cursor = next.as_object_mut().context("rate field parent changed type")?;
    }
    let key = path.last().unwrap();
    match value { Some(value) => { cursor.insert(key.clone(), value); }, None => { cursor.remove(key); } }
    Ok(())
}

fn apply(intent: &Intent, current: &Value, after: Option<i64>, check_conflicts: bool) -> Result<Value> {
    ensure!(!intent.legacy || !check_conflicts, "older rate edits need reconciliation");
    let events = events(intent, after)?;
    let mut result = current.as_object().context("unreadable Cloud rates")?.clone();
    let mut result_value = Value::Object(std::mem::take(&mut result));
    let all: BTreeSet<_> = events.iter().flat_map(|event| event.paths.iter().cloned()).collect();
    let mut paths: Vec<Path> = Vec::new();
    for path in all { if !paths.iter().any(|parent| path.starts_with(parent)) { paths.push(path); } }
    for path in paths {
        let touching: Vec<_> = events.iter().filter(|event| event.paths.iter().any(|changed| changed.starts_with(&path))).collect();
        let first = touching.first().context("missing rate baseline")?;
        let last = touching.last().unwrap();
        let before = get(&first.before, &path); let desired = get(&last.after, &path);
        if equal(&path, before, desired) { continue; }
        let observed = view(&result_value)?;
        if check_conflicts {
            // An absent parent is a concurrent deletion, not an invitation to
            // recreate a plan from one edited leaf.
            for length in 2..path.len() {
                if get(&first.before, &path[..length]).is_some() {
                    ensure!(get(&observed, &path[..length]).is_some(), "rate field parent was removed in Cloud");
                }
            }
            let value = get(&observed, &path);
            ensure!(equal(&path, value, before) || touching.iter().any(|event| equal(&path, value, get(&event.after, &path))),
                "rate field changed in Cloud");
        }
        if !equal(&path, get(&observed, &path), desired) { set(&mut result_value, &path, desired.cloned())?; }
    }
    // A priced Home-freeze label may need publication even when no local
    // value changed. Existing unrelated Cloud plans are never removed here.
    let mut asserted = BTreeSet::new();
    for event in events.iter().filter(|event| event.force && event.paths.is_empty()) {
        for key in ["charging_currency", "charging_default_rate"] {
            if event.after.get(key).is_some() { asserted.insert(vec![key.into()]); }
        }
        for tag in plans(event.after.get("charging_tag_rates"))?.keys() { asserted.insert(vec!["charging_tag_rates".into(), tag.clone()]); }
    }
    if let Some(last) = events.last() {
        for path in asserted {
            let Some(desired) = get(&last.after, &path) else { continue; };
            let observed = view(&result_value)?; let current = get(&observed, &path);
            if check_conflicts {
                ensure!(current.is_none() || equal(&path, current, Some(desired)), "required local rate differs from Cloud");
            }
            if !equal(&path, current, Some(desired)) { set(&mut result_value, &path, Some(desired.clone()))?; }
        }
    }
    // A Home-freeze requirement applies only to its named plan. Existing
    // Cloud metadata survives when the required pricing already agrees.
    let required:BTreeSet<_>=events.iter().filter_map(|event|event.required_tag.as_ref()).collect();
    if let Some(last)=events.last() {
        for tag in required {
            let path=vec!["charging_tag_rates".into(),tag.clone()];
            let Some(desired)=get(&last.after,&path) else {continue};
            let observed=view(&result_value)?;
            let current=get(&observed,&path);
            let agrees=["flat","schedules"].iter().all(|field| {
                let mut field_path=path.clone();field_path.push((*field).into());
                equal(&field_path,current.and_then(|value|value.get(*field)),desired.get(*field))
            });
            if current.is_none() {set(&mut result_value,&path,Some(desired.clone()))?;}
            else if check_conflicts {ensure!(agrees,"required rate plan differs from Cloud");}
            else if !agrees {
                // Preserve newer local pricing during confirmation, together
                // with fields Cloud owns that this local plan never edited.
                for field in ["flat","schedules"] {
                    let mut field_path=path.clone();field_path.push(field.into());
                    set(&mut result_value,&field_path,desired.get(field).cloned())?;
                }
            }
        }
    }
    Ok(result_value)
}

/// The first encrypted publication has no Cloud baseline to merge against.
/// Retain supported legacy representations, but never include other preferences.
pub fn initial_document(local: &Value) -> Result<Value> {
    let fields = local.as_object().context("local rates are not an object")?;
    ensure!(fields.keys().all(|key| RATE_KEYS.contains(&key.as_str())),
        "unrelated preferences cannot enter Cloud rates");
    view(local)?;
    Ok(local.clone())
}

pub fn merge(intent: &Intent, cloud: &Value) -> Result<Value> { apply(intent, cloud, None, true) }
/// Adopt current Cloud fields while retaining explicitly newer local changes.
pub fn replay_after(intent: &Intent, through: i64, cloud: &Value) -> Result<Value> { apply(intent, cloud, Some(through), false) }

#[cfg(test)]
mod tests {
    use super::*;
    fn intent(edits: Vec<(Value, Value)>) -> Intent {
        Intent { legacy: false, edits: edits.into_iter().enumerate().map(|(index, (before, after))|
            (index as i64 + 1, Edit::RateConfig { before, after, force: false, required_tag:None })).collect() }
    }
    #[test]
    fn a_flat_change_preserves_remote_schedules_other_plans_and_extensions() {
        let before = json!({"charging_tag_rates":{"Home":{"flat":0.1}}});
        let after = json!({"charging_tag_rates":{"Home":{"flat":0.2}}});
        let cloud = json!({"charging_currency":"CAD","charging_tag_rates":{"Home":{"flat":0.1,"schedules":[{"rate":0.05}],"extension":true},"Work":0.3},"future":{"keep":true}});
        let merged = merge(&intent(vec![(before, after)]), &cloud).unwrap();
        let mut expected = cloud; expected["charging_tag_rates"]["Home"]["flat"] = json!(0.2);
        assert_eq!(merged, expected);
    }
    #[test]
    fn numeric_strings_and_legacy_flat_plans_share_a_baseline() {
        let changes = intent(vec![(json!({"charging_default_rate":"0.1","charging_tag_rates":{"Home":"0.2"}}),
            json!({"charging_default_rate":0.3,"charging_tag_rates":{"Home":{"flat":0.4,"schedules":[]}}}))]);
        let merged = merge(&changes, &json!({"charging_default_rate":0.1,"charging_tag_rates":{"Home":0.2}})).unwrap();
        assert_eq!(merged["charging_default_rate"], 0.3); assert_eq!(merged["charging_tag_rates"]["Home"]["flat"], 0.4);
    }
    #[test]
    fn competing_prices_schedule_reordering_and_parent_deletion_are_conflicts() {
        let changes = intent(vec![(json!({"charging_tag_rates":{"Home":{"flat":0.1}}}),json!({"charging_tag_rates":{"Home":{"flat":0.2}}}))]);
        assert!(merge(&changes, &json!({"charging_tag_rates":{"Home":{"flat":0.3}}})).is_err());
        assert!(merge(&changes, &json!({"charging_tag_rates":{}})).is_err());
        let changes = intent(vec![(json!({"charging_tag_rates":{"Home":{"schedules":[1,2]}}}),json!({"charging_tag_rates":{"Home":{"schedules":[1,3]}}}))]);
        assert!(merge(&changes, &json!({"charging_tag_rates":{"Home":{"schedules":[2,1]}}})).is_err());
    }
    #[test]
    fn add_then_edit_and_cancelled_edits_collapse_without_replaying_old_values() {
        let a = json!({}); let b = json!({"charging_tag_rates":{"Home":{"flat":0.1}}});
        let c = json!({"charging_tag_rates":{"Home":{"flat":0.2}}});
        let changes = intent(vec![(a.clone(),b.clone()),(b.clone(),c.clone())]);
        assert_eq!(merge(&changes,&a).unwrap(),c); assert_eq!(merge(&changes,&c).unwrap(),c);
        let cancelled = intent(vec![(a.clone(),b.clone()),(b,a)]);
        assert_eq!(merge(&cancelled,&c).unwrap(),c);
    }
    #[test]
    fn deleting_a_plan_cannot_drop_concurrently_added_metadata() {
        let changes=intent(vec![(json!({"charging_tag_rates":{"Home":{"flat":0.1}}}),json!({"charging_tag_rates":{}}))]);
        assert!(merge(&changes,&json!({"charging_tag_rates":{"Home":{"flat":0.1,"new":true}}})).is_err());
    }
    #[test]
    fn confirmation_replays_only_newer_local_fields_over_current_cloud_data() {
        let a=json!({"charging_default_rate":0.1});let b=json!({"charging_default_rate":0.2});let c=json!({"charging_default_rate":0.3});
        let changes=intent(vec![(a,b.clone()),(b,c)]);
        assert_eq!(replay_after(&changes,1,&json!({"charging_default_rate":0.25,"extension":true})).unwrap(),json!({"charging_default_rate":0.3,"extension":true}));
    }
    #[test]
    fn explicit_republication_can_fill_an_absent_plan_but_never_replace_a_competing_price() {
        let desired=json!({"charging_tag_rates":{"Frozen":{"flat":0.2}}});
        let changes=Intent {legacy:false,edits:vec![(1,Edit::RateConfig {before:desired.clone(),after:desired.clone(),force:true,required_tag:None})]};
        assert_eq!(merge(&changes,&json!({})).unwrap(),desired);
        assert!(merge(&changes,&json!({"charging_tag_rates":{"Frozen":0.3}})).is_err());
    }
}

#[cfg(test)]
mod required_plan_tests {
    use super::*;
    fn required(document:Value)->Intent {
        Intent {legacy:false,edits:vec![(1,Edit::RateConfig {before:document.clone(),after:document,
            force:false,required_tag:Some("Old Home".into())})]}
    }
    #[test]
    fn preserved_label_does_not_assert_unrelated_prices_currency_or_other_plans() {
        let local=json!({"charging_currency":"CAD","charging_default_rate":0.1,"charging_tag_rates":{"Old Home":0.2,"Work":0.4}});
        let cloud=json!({"charging_currency":"USD","charging_default_rate":0.3,"charging_tag_rates":{"Work":0.5},"future":true});
        let mut expected=cloud.clone();expected["charging_tag_rates"]["Old Home"]=json!({"flat":0.2});
        assert_eq!(merge(&required(local),&cloud).unwrap(),expected);
    }
    #[test]
    fn matching_price_preserves_remote_extensions_but_conflicting_price_or_schedule_refuses() {
        let intent=required(json!({"charging_tag_rates":{"Old Home":0.2}}));
        let same=json!({"charging_tag_rates":{"Old Home":{"flat":"0.20","extension":true}}});
        assert_eq!(merge(&intent,&same).unwrap(),same);
        assert!(merge(&intent,&json!({"charging_tag_rates":{"Old Home":0.3}})).is_err());
        assert!(merge(&intent,&json!({"charging_tag_rates":{"Old Home":{"flat":0.2,"schedules":[{"rate":0.1}]}}})).is_err());
    }
    #[test]
    fn later_deletion_cancels_requirement_and_confirmation_preserves_newer_requirement() {
        let original=json!({"charging_tag_rates":{"Old Home":0.2}});let mut intent=required(original.clone());
        intent.edits.push((2,Edit::RateConfig {before:original,after:json!({"charging_tag_rates":{}}),force:false,required_tag:None}));
        assert_eq!(merge(&intent,&json!({"charging_tag_rates":{"Old Home":0.2,"Work":0.3}})).unwrap(),json!({"charging_tag_rates":{"Work":0.3}}));
        let mut intent=required(json!({"charging_tag_rates":{"Old Home":0.2}}));intent.edits[0].0=2;
        assert_eq!(replay_after(&intent,1,&json!({"charging_tag_rates":{"Old Home":{"flat":0.3,"extension":true}},"charging_currency":"CAD"})).unwrap(),
            json!({"charging_tag_rates":{"Old Home":{"flat":0.2,"extension":true}},"charging_currency":"CAD"}));
    }
    #[test]
    fn older_serialized_intent_remains_readable_and_conservative() {
        let parsed:Edit=serde_json::from_value(json!({"field":"rateConfig","before":{},"after":{},"force":true})).unwrap();
        assert!(matches!(parsed,Edit::RateConfig {required_tag:None,force:true,..}));
    }
}
