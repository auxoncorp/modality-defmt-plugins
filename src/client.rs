use crate::Error;
use modality_api::{AttrVal, TimelineId};
use modality_ingest_client::dynamic::DynamicIngestClient;
use modality_ingest_client::{IngestClient, ReadyState};
use modality_ingest_protocol::InternedAttrKey;
use std::collections::BTreeMap;

pub struct Client {
    timeline_keys: BTreeMap<String, InternedAttrKey>,
    event_keys: BTreeMap<String, InternedAttrKey>,
    inner: DynamicIngestClient,
}

impl Client {
    pub fn new(client: IngestClient<ReadyState>) -> Self {
        Self {
            timeline_keys: Default::default(),
            event_keys: Default::default(),
            inner: client.into(),
        }
    }

    pub async fn switch_timeline(
        &mut self,
        id: TimelineId,
        new_timeline_attrs: Option<impl IntoIterator<Item = (&String, &AttrVal)>>,
    ) -> Result<(), Error> {
        self.inner.open_timeline(id).await?;
        if let Some(attrs) = new_timeline_attrs {
            let mut interned_attrs = Vec::new();
            for (k, v) in attrs.into_iter() {
                let key = normalize_timeline_key(k);
                let int_key = if let Some(ik) = self.timeline_keys.get(&key) {
                    *ik
                } else {
                    let ik = self.inner.declare_attr_key(key.clone()).await?;
                    self.timeline_keys.insert(key, ik);
                    ik
                };
                interned_attrs.push((int_key, v.clone()));
            }
            self.inner.timeline_metadata(interned_attrs).await?;
        }
        Ok(())
    }

    pub async fn send_event(
        &mut self,
        ordering: u128,
        attrs: impl IntoIterator<Item = (&String, &AttrVal)>,
    ) -> Result<(), Error> {
        let mut interned_attrs = Vec::new();
        for (k, v) in attrs.into_iter() {
            let key = normalize_event_key(k);
            let int_key = if let Some(ik) = self.timeline_keys.get(&key) {
                *ik
            } else {
                let ik = self.inner.declare_attr_key(key.clone()).await?;
                self.event_keys.insert(key, ik);
                ik
            };
            interned_attrs.push((int_key, v.clone()));
        }
        self.inner.event(ordering, interned_attrs).await?;
        Ok(())
    }
}

fn normalize_timeline_key(s: &str) -> String {
    if s.starts_with("timeline.") {
        s.to_owned()
    } else {
        format!("timeline.{s}")
    }
}

fn normalize_event_key(s: &str) -> String {
    if s.starts_with("event.") {
        s.to_owned()
    } else {
        format!("event.{s}")
    }
}
