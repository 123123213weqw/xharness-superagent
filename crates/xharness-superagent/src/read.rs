//! Management and source-poll read queries, matching the pinned schema-11 store.
use crate::{
    store::{escape_like, Store},
    Error, Result,
};
use serde_json::{json, Value};
fn text<'a>(args: &'a [Value], index: usize, default: &'a str) -> &'a str {
    args.get(index).and_then(Value::as_str).unwrap_or(default)
}
fn number(args: &[Value], index: usize, default: i64) -> i64 {
    args.get(index).and_then(Value::as_i64).unwrap_or(default)
}
fn boolean(args: &[Value], index: usize) -> bool {
    args.get(index).and_then(Value::as_bool).unwrap_or(false)
}
fn page(mut items: Vec<Value>, limit: usize) -> Value {
    let more = items.len() > limit;
    items.truncate(limit);
    json!({"items":items,"more":more})
}
impl Store {
    /// Named read operations are also the stable transport boundary used by the panel.
    pub fn read(&self, method: &str, args: &[Value]) -> Result<Value> {
        Ok(match method {
            "recentTriggers"|"recentDecisions"=>{
                let table=if method=="recentTriggers"{"trigger"}else{"decision"};
                json!(self.values::<Value>(&format!("SELECT value FROM {table} ORDER BY rowid DESC LIMIT ? OFFSET ?"),&[json!(number(args,1,50)),json!(number(args,0,0))])?)
            },
            "triggerStats"=>{
                let since=text(args,0,"");let rows=self.query("SELECT json_extract(t.value,'$.source') AS source, MAX(t.at) AS last, SUM(t.at>=?) AS received, SUM(t.at>=? AND json_extract(d.value,'$.filtered') IS NOT NULL) AS filtered FROM trigger t LEFT JOIN disposition d ON d.id=t.id GROUP BY source",&[json!(since),json!(since)])?;
                let mut result=serde_json::Map::new();for row in rows{result.insert(row["source"].as_str().unwrap_or("").into(),json!({"received":row["received"].as_i64().unwrap_or(0),"filtered":row["filtered"].as_i64().unwrap_or(0),"lastAt":row["last"].as_str().unwrap_or("")}));}Value::Object(result)
            },
            "flow"=>{
                let since=text(args,0,"");
                let judged=self.query("SELECT COUNT(DISTINCT t.id) AS n FROM decision d, json_each(json_extract(d.value,'$.triggerIds')) j JOIN trigger t ON t.id=j.value WHERE t.at>=? AND NOT EXISTS(SELECT 1 FROM disposition s WHERE s.id=t.id AND json_extract(s.value,'$.filtered') IS NOT NULL)",&[json!(since)])?;
                let accepted=self.query("SELECT COUNT(DISTINCT d.id) AS n FROM decision d, json_each(json_extract(d.value,'$.triggerIds')) j JOIN trigger t ON t.id=j.value WHERE json_extract(d.value,'$.kind') IN ('accept','reopen') AND t.at>=?",&[json!(since)])?;
                json!({"judged":judged[0]["n"],"accepted":accepted[0]["n"]})
            },
            "triggers"=>{
                let options=args.first().cloned().unwrap_or_else(||json!({}));let mut conditions=Vec::new();let mut params=Vec::new();
                if let Some(source)=options["source"].as_str().filter(|s|!s.is_empty()){conditions.push("json_extract(t.value,'$.source')=?");params.push(json!(source));}
                if options["filtered"]==true{conditions.push("json_extract(d.value,'$.filtered') IS NOT NULL");}
                let clause=if conditions.is_empty(){String::new()}else{format!("WHERE {}",conditions.join(" AND "))};let limit=number(args,2,50);
                params.extend([json!(limit+1),json!(number(args,1,0))]);page(self.values(&format!("SELECT t.value FROM trigger t LEFT JOIN disposition d ON d.id=t.id {clause} ORDER BY t.at DESC LIMIT ? OFFSET ?"),&params)?,limit.max(0) as usize)
            },
            "threadTriggers"=>{
                let before=args.get(2).filter(|v|v.is_object());let mut params=vec![json!(text(args,0,""))];
                let place=if let Some(place)=before{params.extend([place["at"].clone(),place["id"].clone()]);format!("AND (x.at,x.id) {} (?,?)",if boolean(args,3){"<="}else{"<"})}else{String::new()};params.push(json!(number(args,1,50)));
                let mut items=self.values::<Value>(&format!("SELECT t.value FROM trigger_thread x JOIN trigger t ON t.id=x.id WHERE x.thread=? {place} ORDER BY x.at DESC,x.id DESC LIMIT ?"),&params)?;items.reverse();json!(items)
            },
            "threadPage"=>json!(self.values::<Value>("SELECT t.value FROM trigger_thread x JOIN trigger t ON t.id=x.id WHERE x.thread=? ORDER BY x.at DESC,t.rowid DESC LIMIT ? OFFSET ?",&[json!(text(args,0,"")),json!(number(args,2,50)),json!(number(args,1,0))])?),
            "neighbours"=>{
                let mut result=serde_json::Map::new();for (name,side,order) in [("before","<","DESC"),("after",">","ASC")]{
                    if let Some(value)=self.values::<Value>(&format!("SELECT t.value FROM trigger_thread x JOIN trigger t ON t.id=x.id WHERE x.thread=? AND (x.at,x.id) {side} (?,?) AND json_extract(t.value,'$.meta.ci') IS NULL ORDER BY x.at {order},x.id {order} LIMIT 1"),&[json!(text(args,0,"")),json!(text(args,1,"")),json!(text(args,2,""))])?.into_iter().next(){result.insert(name.into(),value);}
                }Value::Object(result)
            },
            "firstTrigger"=>self.values::<Value>(&format!("SELECT t.value FROM trigger_thread x JOIN trigger t ON t.id=x.id WHERE x.thread=? {} ORDER BY x.at ASC,t.rowid ASC LIMIT 1",if boolean(args,1){"AND x.human=1"}else{""}),&[json!(text(args,0,""))])?.into_iter().next().unwrap_or(Value::Null),
            "search"=>{
                let query=text(args,0,"");json!(self.values::<Value>("SELECT value FROM trigger WHERE id=? OR json_extract(value,'$.url') LIKE ? ESCAPE '\\' OR json_extract(value,'$.meta.messageId')=? OR json_extract(value,'$.text') LIKE ? ESCAPE '\\' ORDER BY at DESC LIMIT ?",&[json!(query),json!(format!("{}%",escape_like(query))),json!(query),json!(format!("%{}%",escape_like(query))),json!(number(args,1,20))])?)
            },
            "decisions"|"decisionsOfKinds"=>{
                let (kinds,since,offset,limit)=if method=="decisions"{let options=args.first().cloned().unwrap_or_else(||json!({}));(options["kinds"].as_array().cloned().unwrap_or_default(),options["since"].as_str().unwrap_or("").to_owned(),number(args,1,0),number(args,2,50))}else{(args.first().and_then(Value::as_array).cloned().unwrap_or_default(),String::new(),0,number(args,1,30))};
                let mut conditions=Vec::new();let mut params=kinds.clone();
                if !kinds.is_empty()||method=="decisionsOfKinds"{conditions.push(format!("json_extract(d.value,'$.kind') IN ({})",vec!["?";kinds.len()].join(",")));}
                if !since.is_empty(){conditions.push("EXISTS(SELECT 1 FROM json_each(json_extract(d.value,'$.triggerIds')) j JOIN trigger t ON t.id=j.value WHERE t.at>=?)".into());params.push(json!(since));}
                let clause=if conditions.is_empty(){String::new()}else{format!("WHERE {}",conditions.join(" AND "))};
                params.extend([json!(if method=="decisions"{limit+1}else{limit}),json!(offset)]);
                let items=self.values::<Value>(&format!("SELECT d.value FROM decision d {clause} ORDER BY d.rowid DESC LIMIT ? OFFSET ?"),&params)?;
                if method=="decisions"{page(items,limit.max(0) as usize)}else{json!(items)}
            },
            "failingReplies"=>{
                let rows=self.query("SELECT o.value,e.attempts,e.error FROM effect e JOIN outbox o ON o.id=e.key WHERE e.kind='reply' AND e.attempts>0 ORDER BY e.seq",&[])?;
                let mut result=Vec::new();for row in rows{let mut value:Value=serde_json::from_str(row["value"].as_str().unwrap_or(""))?;value["attempts"]=row["attempts"].clone();value["error"]=row["error"].clone();result.push(value);}json!(result)
            },
            "droppedReplies"=>json!(self.values::<Value>("SELECT value FROM outbox WHERE dropped IS NOT NULL ORDER BY dropped DESC LIMIT ?",&[json!(number(args,0,20))])?),
            "droppedSince"=>self.query("SELECT COUNT(*) AS n FROM outbox WHERE dropped>=?",&[json!(text(args,0,""))])?[0]["n"].clone(),
            "subscribedThreads"=>json!(if !text(args,0,"").is_empty(){self.values::<Value>("SELECT value FROM thread WHERE subscribed=? ORDER BY rowid",&[json!(text(args,0,""))])?}else{self.values::<Value>("SELECT value FROM thread INDEXED BY thread_subscribed WHERE subscribed IS NOT NULL ORDER BY rowid",&[])?}),
            _=>return Err(Error::Message(format!("Unknown store read: {method}"))),
        })
    }
}
