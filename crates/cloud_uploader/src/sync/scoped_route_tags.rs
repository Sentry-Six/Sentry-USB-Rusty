//! Shared native frame-scoped tag operations; wire format is unchanged.
pub(super) use sentryusb_drives::scoped_tags::{apply_delta,tags_for_ranges};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value,json};
    #[test]
    fn cross_implementation_vectors_preserve_neighboring_drives() {
        let vectors:Value=serde_json::from_str(include_str!("../../test-vectors/scoped-route-tags.json")).unwrap();
        for vector in vectors.as_array().unwrap() {
            let ranges:Vec<(u32,u32)>=serde_json::from_value(vector["ranges"].clone()).unwrap();
            let added:Vec<String>=serde_json::from_value(vector["added"].clone()).unwrap();
            let removed:Vec<String>=serde_json::from_value(vector["removed"].clone()).unwrap();
            let total:u32=serde_json::from_value(vector["totalFrames"].clone()).unwrap();
            let result=apply_delta(&vector["before"],total,&ranges,&added,&removed).unwrap();
            assert_eq!(result,vector["after"],"{}",vector["name"]);
            for check in vector["views"].as_array().unwrap() {
                let selection:Vec<(u32,u32)>=serde_json::from_value(check["ranges"].clone()).unwrap();
                assert_eq!(serde_json::to_value(tags_for_ranges(&result,total,&selection).unwrap()).unwrap(),check["tags"],"{}",vector["name"]);
            }
        }
    }
    #[test]
    fn browser_encrypted_scoped_document_uses_the_existing_route_key_and_aad() {
        use base64::{Engine as _,engine::general_purpose::STANDARD as B64};
        use sentryusb_cloud_crypto::aad;
        let vector:Value=serde_json::from_str(include_str!("../../test-vectors/scoped-route-tags-encrypted.json")).unwrap();
        let user=vector["userId"].as_str().unwrap();let pi=vector["piId"].as_str().unwrap();let id=vector["routeId"].as_str().unwrap();
        let pi_key:[u8;32]=B64.decode(vector["piKeyB64"].as_str().unwrap()).unwrap().try_into().unwrap();
        let content=crate::encrypt::unwrap_content_key(&pi_key,vector["wrappedRouteKey"].as_str().unwrap(),&aad::route_key(user,pi,id)).unwrap();
        let decoded:Value=crate::encrypt::open_json_b64(&content,&aad::route_tags(user,pi,id),vector["tagsCiphertext"].as_str().unwrap()).unwrap();
        assert_eq!(decoded,vector["document"]);
        assert!(tags_for_ranges(&decoded,60,&[(0,20)]).unwrap().is_empty());
        assert_eq!(tags_for_ranges(&decoded,60,&[(25,40)]).unwrap(),vec!["Work"]);
        assert!(crate::encrypt::open_json_b64::<Value>(&content,&aad::route_tags(user,"another-pi",id),vector["tagsCiphertext"].as_str().unwrap()).is_err());
    }

    #[test]
    fn malformed_and_different_source_documents_do_not_become_empty_tags() {
        for value in [json!({"version":3}),json!({"version":2,"totalFrames":61,"defaultTags":[],"spans":[]}),
            json!({"version":2,"totalFrames":60,"defaultTags":[],"spans":[{"startFrame":0,"endFrame":30,"tags":[]},{"startFrame":20,"endFrame":40,"tags":[]}]}),
            json!({"version":2,"totalFrames":60,"defaultTags":[],"spans":[{"startFrame":0,"endFrame":30,"tags":[],"future":true}]})] {
            assert!(tags_for_ranges(&value,60,&[(0,20)]).is_err());
        }
        assert!(apply_delta(&json!([]),60,&[(20,20)],&[],&[]).is_err());
        assert!(apply_delta(&json!([]),60,&[(0,20)],&["Work".into()],&["Work".into()]).is_err());
    }
}
