//! Scoped tag documents preserve independent drives sharing one source clip.
//! Legacy arrays are defaults; version 2 overrides half-open frame ranges.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn tags(value: &Value) -> Result<Vec<String>> {
    let items=value.as_array().context("invalid scoped tag list")?;
    ensure!(items.len()<=128,"too many scoped tags");
    let mut tags=BTreeSet::new();
    for item in items {
        let tag=item.as_str().context("invalid scoped tag")?;
        ensure!(!tag.is_empty() && tag.encode_utf16().count()<=64,"invalid scoped tag length");
        tags.insert(tag.to_string());
    }
    Ok(tags.into_iter().collect())
}
fn frame(value: &Value) -> Result<u32> {
    u32::try_from(value.as_u64().context("invalid frame")?).context("frame out of range")
}
fn range(start: u32,end: u32,total: u32) -> Result<()> {
    ensure!(start<end && end<=total,"invalid frame range"); Ok(())
}
#[derive(Clone)]
struct Span {start: u32,end: u32,tags: Vec<String>}
struct Document {original: Value,total: u32,default: Vec<String>,spans: Vec<Span>}
impl Document {
    fn parse(value: &Value,total: u32) -> Result<Self> {
        ensure!(total>0,"empty source frame range");
        if value.is_array() {
            let default=tags(value)?;
            return Ok(Self {original:json!({"version":2,"totalFrames":total,"defaultTags":default,"spans":[]}),total,default,spans:vec![]})
        }
        ensure!(value.is_object() && value["version"]==2 && frame(&value["totalFrames"])?==total,"scoped tag source/version mismatch");
        let default=tags(&value["defaultTags"])?;
        let raw=value["spans"].as_array().context("missing scoped tag spans")?;
        ensure!(raw.len()<=256,"too many scoped tag spans");
        let mut spans=Vec::new();let mut prior_end=0;
        for item in raw {
            let object=item.as_object().context("invalid scoped tag span")?;
            ensure!(object.keys().all(|key|matches!(key.as_str(),"startFrame"|"endFrame"|"tags")),"unsupported scoped tag span field");
            let start=frame(&item["startFrame"])?;let end=frame(&item["endFrame"])?;
            range(start,end,total)?;ensure!(start>=prior_end,"overlapping scoped tag spans");prior_end=end;
            spans.push(Span {start,end,tags:tags(&item["tags"])?});
        }
        Ok(Self {original:value.clone(),total,default,spans})
    }
    fn at(&self,frame: u32)->&Vec<String> {
        self.spans.iter().find(|span|span.start<=frame && frame<span.end).map(|span|&span.tags).unwrap_or(&self.default)
    }
    fn cuts(&self,ranges: &[(u32,u32)])->Result<Vec<u32>> {
        ensure!(!ranges.is_empty() && ranges.len()<=256,"invalid scoped tag selection");
        let mut cuts=BTreeSet::from([0,self.total]);
        for span in &self.spans {cuts.insert(span.start);cuts.insert(span.end);}
        for &(start,end) in ranges {range(start,end,self.total)?;cuts.insert(start);cuts.insert(end);}
        Ok(cuts.into_iter().collect())
    }
}

pub fn tags_for_ranges(value: &Value,total: u32,ranges: &[(u32,u32)])->Result<Vec<String>> {
    let doc=Document::parse(value,total)?;let cuts=doc.cuts(ranges)?;let mut result=BTreeSet::new();
    for window in cuts.windows(2) {
        if ranges.iter().any(|&(start,end)|start<=window[0] && window[0]<end) {result.extend(doc.at(window[0]).iter().cloned());}
    }
    Ok(result.into_iter().collect())
}

pub fn apply_delta(value: &Value,total: u32,ranges: &[(u32,u32)],added: &[String],removed: &[String])->Result<Value> {
    let doc=Document::parse(value,total)?;let cuts=doc.cuts(ranges)?;
    let added=tags(&serde_json::to_value(added)?)?;let removed:BTreeSet<_>=tags(&serde_json::to_value(removed)?)?.into_iter().collect();
    ensure!(added.iter().all(|tag|!removed.contains(tag)),"contradictory scoped tag edit");
    let mut spans:Vec<Span>=Vec::new();
    for window in cuts.windows(2) {
        let (start,end)=(window[0],window[1]);let current=doc.at(start);
        let next:Vec<String>=if ranges.iter().any(|&(a,b)|a<=start && start<b) {
            current.iter().filter(|tag|!removed.contains(*tag)).cloned().chain(added.iter().cloned()).collect::<BTreeSet<_>>().into_iter().collect()
        } else {current.clone()};
        if next==doc.default {continue}
        if let Some(previous)=spans.last_mut().filter(|span|span.end==start && span.tags==next) {previous.end=end;}
        else {spans.push(Span {start,end,tags:next});}
    }
    ensure!(spans.len()<=256,"too many scoped tag spans");
    let mut result=doc.original;
    result["defaultTags"]=serde_json::to_value(doc.default)?;
    result["spans"]=Value::Array(spans.iter().map(|span|json!({"startFrame":span.start,"endFrame":span.end,"tags":span.tags})).collect());
    Ok(result)
}
