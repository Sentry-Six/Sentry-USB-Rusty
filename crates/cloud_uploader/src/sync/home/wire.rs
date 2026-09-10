use super::*;
use base64::{Engine as _,engine::general_purpose::URL_SAFE_NO_PAD};

pub(super) fn revision(value:&str)->Result<i64> {
    ensure!(!value.is_empty() && (value=="0" || !value.starts_with('0')) && value.bytes().all(|c|c.is_ascii_digit()),"invalid Home revision");
    value.parse::<i64>().context("invalid Home revision")
}
pub(super) fn hex(value:&str)->bool {value.len()==64 && value.bytes().all(|c|c.is_ascii_digit()||(b'a'..=b'f').contains(&c))}
pub(super) fn id(value:&Value)->Result<&str> {
    ensure!(value["kind"]=="charge","invalid Home source kind");
    let id=value["id"].as_str().context("missing Home source id")?;
    ensure!(hex(id),"invalid Home source id");Ok(id)
}
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct Cursor {v:u8,user_id:String,pi_id:String,generation:u32,since:Option<String>,revision:String,at:String,id:String}
fn cursor(raw:&str,creds:&CloudCredentialsV1,since:Option<&str>)->Result<Cursor> {
    ensure!(raw.len()<=2048,"Home cursor is too large");
    let bytes=URL_SAFE_NO_PAD.decode(raw)?;ensure!(URL_SAFE_NO_PAD.encode(&bytes)==raw,"noncanonical Home cursor");
    let c:Cursor=serde_json::from_slice(&bytes)?;
    ensure!(c.v==1 && c.user_id==creds.user_id && c.pi_id==creds.pi_id && c.generation==creds.dek_rotation_generation
        && c.since.as_deref()==since && hex(&c.id),"Home cursor source changed");
    ensure!(revision(&c.at)?<=revision(&c.revision)? && since.map(revision).transpose()?.is_none_or(|since|revision(&c.at).is_ok_and(|at|at>since)),"invalid Home cursor position");Ok(c)
}
pub(super) struct Page {pub items:Vec<Value>,pub next:Option<String>,pub revision:String}
pub(super) async fn request(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,binding:&str,path:&str,body:Value)->Result<Value> {
    ensure!(body["piId"]==creds.pi_id && body["dekRotationGeneration"]==creds.dek_rotation_generation,"Home request credentials changed");
    {let _guard=super::super::revision::current_pairing(state,binding).await?;}
    let response=client.post_json_bearer(path,&body).await?;
    let status=response.status();
    let value:Value=serde_json::from_slice(&super::super::revision::bounded_body(response).await?)?;
    if status.as_u16()==409 && value["error"]=="sync_reset_required" {return Err(Reset.into())}
    ensure!(status.is_success(),"Home sync request failed ({status})");
    ensure!(value["ok"]==true && value["writeProtocol"]==3,"invalid Home sync response");
    {let _guard=super::super::revision::current_pairing(state,binding).await?;}Ok(value)
}
pub(super) async fn page(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,binding:&str,walk:&Walk)->Result<Page> {
    let mut body=json!({"piId":creds.pi_id,"dekRotationGeneration":creds.dek_rotation_generation,"limit":100});
    if let Some(since)=&walk.since {revision(since)?;body["since"]=json!(since)}
    let previous=walk.cursor.as_deref().map(|raw|cursor(raw,creds,walk.since.as_deref())).transpose()?;
    if let Some(cursor)=&walk.cursor {body["cursor"]=json!(cursor)}
    let value=request(state,client,creds,binding,"/api/pi/sync/charge-states",body).await?;
    ensure!(value["sync"]["version"]==1,"invalid Home page version");
    let upper=value["sync"]["revision"].as_str().context("missing Home page revision")?.to_owned();let upper_number=revision(&upper)?;
    ensure!(previous.as_ref().is_none_or(|p|p.revision==upper) && walk.since.as_deref().map(revision).transpose()?.is_none_or(|since|since<=upper_number),"Home page revision changed");
    let items=value["items"].as_array().context("missing Home page items")?.clone();ensure!(items.len()<=100,"oversized Home page");
    let mut last=previous.map(|p|->Result<_>{Ok((revision(&p.at)?,p.id))}).transpose()?;
    let mut seen=BTreeSet::new();
    for item in &items {
        let id=id(item)?;ensure!(seen.insert(id),"duplicate Home source");ensure!(item["status"]=="ok","invalid Home page item");
        let at=revision(item["revision"].as_str().context("missing Home source revision")?)?;
        ensure!(at<=upper_number && walk.since.as_deref().map(revision).transpose()?.is_none_or(|since|at>since)
            && last.as_ref().is_none_or(|(old,old_id)| (at,id)>(*old,old_id.as_str())),"Home page did not advance");last=Some((at,id.into()));
    }
    let next=match value.get("nextCursor") {Some(Value::Null)=>None,Some(Value::String(s))=>Some(s.clone()),_=>anyhow::bail!("missing Home continuation")};
    if let Some(raw)=&next {
        ensure!(!items.is_empty(),"empty continuing Home page");let c=cursor(raw,creds,walk.since.as_deref())?;
        ensure!(c.revision==upper && Some((revision(&c.at)?,c.id))==last && walk.cursor.as_ref()!=Some(raw),"Home cursor did not match its page");
    }
    Ok(Page {items,next,revision:upper})
}
pub(super) async fn targets(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,binding:&str,ids:&[String])->Result<Vec<Value>> {
    let value=request(state,client,creds,binding,"/api/pi/sync/state",json!({"piId":creds.pi_id,"dekRotationGeneration":creds.dek_rotation_generation,
        "items":ids.iter().map(|id|json!({"kind":"charge","id":id})).collect::<Vec<_>>()})).await?;
    let items=value["items"].as_array().context("missing Home retry states")?;let wanted:BTreeSet<_>=ids.iter().map(String::as_str).collect();let mut seen=BTreeSet::new();
    ensure!(items.len()==ids.len() && items.iter().all(|value|id(value).is_ok_and(|id|wanted.contains(id)&&seen.insert(id))) && seen.len()==ids.len(),"incomplete Home retry states");Ok(items.clone())
}
