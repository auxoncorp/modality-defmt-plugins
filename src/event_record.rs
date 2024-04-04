use crate::Error;
use defmt_decoder::{Arg, Frame, Location};
use defmt_parser::{Fragment, ParserMode};
use modality_api::Uuid;
use modality_api::{AttrVal, BigInt, Nanoseconds, TimelineId};
use std::collections::BTreeMap;
use tracing::{debug, warn};

pub type EventAttributes = BTreeMap<String, AttrVal>;

#[derive(Debug)]
pub struct EventRecord {
    attributes: EventAttributes,
}

impl EventRecord {
    const ATTR_KEY_PREFIX: &'static str = "event.";
    const INTERNAL_ATTR_KEY_PREFIX: &'static str = "event.internal.defmt.";

    pub(crate) fn attr_key(k: &str) -> String {
        format!("{}{k}", Self::ATTR_KEY_PREFIX)
    }

    pub(crate) fn internal_attr_key(k: &str) -> String {
        format!("{}{k}", Self::INTERNAL_ATTR_KEY_PREFIX)
    }

    pub(crate) fn new(attributes: EventAttributes) -> Self {
        Self { attributes }
    }

    #[cfg(test)]
    pub(crate) fn from_iter(attrs: impl IntoIterator<Item = (String, AttrVal)>) -> Self {
        Self {
            attributes: attrs.into_iter().collect(),
        }
    }

    pub(crate) fn insert_attr<V: Into<AttrVal>>(&mut self, k: String, v: V) {
        self.attributes.insert(k, v.into());
    }

    pub(crate) fn add_interaction(
        &mut self,
        interactions_enabled: bool,
        remote_tid: TimelineId,
        remote_nonce: i64,
    ) {
        let (rem_tid, rem_nonce) = if interactions_enabled {
            (
                Self::attr_key("interaction.remote_timeline_id"),
                Self::attr_key("interaction.remote_nonce"),
            )
        } else {
            (
                Self::internal_attr_key("interaction.remote_timeline_id"),
                Self::internal_attr_key("interaction.remote_nonce"),
            )
        };

        self.attributes.insert(rem_tid, remote_tid.into());
        self.attributes.insert(rem_nonce, remote_nonce.into());
    }

    pub(crate) fn add_internal_nonce(&mut self, nonce: i64) {
        self.attributes
            .insert(Self::internal_attr_key("nonce"), nonce.into());
    }

    pub(crate) fn promote_internal_nonce(&mut self) {
        if let Some(nonce) = self.attributes.remove("event.internal.defmt.nonce") {
            self.attributes.insert(Self::attr_key("nonce"), nonce);
        }
    }

    pub(crate) fn event_name(&self) -> Option<&str> {
        let v = self.attributes.get("event.name")?;
        if let AttrVal::String(s) = v {
            Some(s.as_ref())
        } else {
            None
        }
    }

    pub(crate) fn task_name(&self) -> Option<&str> {
        let v = self.attributes.get("event.task")?;
        if let AttrVal::String(s) = v {
            Some(s.as_ref())
        } else {
            None
        }
    }

    pub(crate) fn isr_name(&self) -> Option<&str> {
        let v = self.attributes.get("event.isr")?;
        if let AttrVal::String(s) = v {
            Some(s.as_ref())
        } else {
            None
        }
    }

    pub(crate) fn integration_version(&self) -> Option<u16> {
        let v = self.attributes.get("event.version")?;
        if let AttrVal::Integer(version) = v {
            Some(*version as u16)
        } else {
            None
        }
    }

    pub(crate) fn timestamp_raw(&self) -> Option<u64> {
        let v = self.attributes.get("event.internal.defmt.timestamp")?;
        // We only support u64-friendly timestamps atm
        Some(match v {
            AttrVal::BigInt(v) => *v.as_ref() as u64,
            AttrVal::Integer(v) => *v as u64,
            _ => return None,
        })
    }

    #[cfg(test)]
    pub(crate) fn internal_nonce(&self) -> Option<i64> {
        let v = self.attributes.get("event.internal.defmt.nonce")?;
        if let AttrVal::Integer(n) = v {
            Some(*n)
        } else {
            None
        }
    }

    pub fn attributes(&self) -> &EventAttributes {
        &self.attributes
    }

    pub fn from_frame(f: Frame<'_>, location: Option<&Location>) -> Result<Self, Error> {
        let fragments = defmt_parser::parse(f.format(), ParserMode::ForwardsCompatible)?;

        let mut attributes = BTreeMap::default();
        let mut name = None;
        let mut pending_attr_key = None;

        let formatted_string = f.format_args(f.format(), f.args(), None).replace('\n', " ");

        if let Some(ts) = Timestamp::from_frame(&f) {
            attributes.insert(
                Self::internal_attr_key("timestamp.type"),
                ts.typ_str().into(),
            );
            attributes.insert(Self::internal_attr_key("timestamp"), ts.as_u64().into());
            if let Some(ns) = ts.as_nanoseconds() {
                attributes.insert(Self::attr_key("timestamp"), ns.into());
            }
        }

        if let Some(loc) = location {
            attributes.insert(
                Self::attr_key("source.file"),
                loc.file.display().to_string().into(),
            );
            attributes.insert(Self::attr_key("source.line"), loc.line.into());
            attributes.insert(Self::attr_key("source.module"), loc.module.clone().into());
            attributes.insert(
                Self::attr_key("source.uri"),
                format!("file://{}:{}", loc.file.display(), loc.line).into(),
            );
        }

        if let Some(level) = f.level() {
            attributes.insert(Self::attr_key("level"), level.as_str().into());
        }
        attributes.insert(Self::internal_attr_key("table_index"), f.index().into());
        attributes.insert(
            Self::internal_attr_key("formatted_string"),
            formatted_string.clone().into(),
        );

        let mut deviant_event = None;

        for (frag_idx, frag) in fragments.iter().enumerate() {
            match frag {
                Fragment::Literal(l) => {
                    let mut s: &str = l.as_ref();
                    // Look for <event_name>:: convention
                    if frag_idx == 0 {
                        if let Some((n, rem)) = s.split_once("::") {
                            let ev_name = n.trim();
                            deviant_event = DeviantEventKind::from_event_name(ev_name);
                            name = ev_name.to_owned().into();
                            s = rem;
                        }
                    }

                    // Look for literal key/value pairs
                    for (k, v) in extract_literal_key_value_pairs(s).into_iter() {
                        attributes.insert(Self::attr_key(&k), v);
                    }

                    // Look for attribute keys that'll have parameter values.
                    // defmt will yield literal-param pairs in order, so if we
                    // have a param value, it's literal key will be last
                    // (after any literal key/value pairs)
                    s = s.trim_start_matches(',');
                    if let Some((_, rest)) = s.rsplit_once(',') {
                        s = rest;
                    }
                    if let Some((k, _)) = s.split_once('=') {
                        let key = k.trim();
                        pending_attr_key = Some(key);
                    }
                }
                Fragment::Parameter(p) => {
                    if let Some(key) = pending_attr_key.take() {
                        // Normalize the literal in case of multi-token with spaces
                        let key = key.replace(' ', "_");

                        let mut key_type = key.clone();
                        key_type.push_str(".type");
                        attributes.insert(
                            Self::internal_attr_key(&key_type),
                            format!("{:?}", p.ty).to_lowercase().into(),
                        );

                        // SAFETY: decoder/frame already checks args and params
                        let arg = &f.args()[p.index];
                        match arg_to_attr_val(arg) {
                            Some(val) => {
                                attributes.insert(Self::attr_key(&key), val);
                            }
                            None if deviant_event.is_none() => {
                                warn!(
                                    formatted_string,
                                    attr_key = key,
                                    ty = ?p.ty,
                                    "Unsupported arg type"
                                );
                            }
                            None => {
                                // We have a deviant event, special case handle the UUID slices
                                match key.as_ref() {
                                    "mutator.id" | "mutation.id" => {
                                        if let Arg::Slice(uuid_bytes) = arg {
                                            if let Ok(uuid) = Uuid::try_from(uuid_bytes.clone()) {
                                                debug!(attr_key = key, attr_val = %uuid, "Found Deviant attribute");
                                                attributes.insert(
                                                    Self::attr_key(&key),
                                                    uuid_to_integer_attr_val(&uuid),
                                                );
                                            } else {
                                                warn!(attr_key = key, "Invalid UUID bytes");
                                            }
                                        } else {
                                            warn!(
                                                attr_key = key,
                                                "Unsupported argument type for Deviant event"
                                            );
                                        }
                                    }
                                    _ => (),
                                }
                            }
                        }
                    }
                }
            }
        }

        // Use formatted string as event name if we don't have an explicit one
        if let Some(event_name) = name {
            attributes.insert(Self::attr_key("name"), event_name.into());
        } else {
            attributes.insert(Self::attr_key("name"), formatted_string.clone().into());
        }

        Ok(EventRecord { attributes })
    }
}

// TODO - support nested variants and destructuring
fn arg_to_attr_val(arg: &Arg) -> Option<AttrVal> {
    Some(match arg {
        Arg::Bool(v) => (*v).into(),
        Arg::F32(v) => (*v).into(),
        Arg::F64(v) => (*v).into(),
        // NOTE: we only support i128 currently
        Arg::Uxx(v) => BigInt::new_attr_val(*v as i128),
        Arg::Ixx(v) => BigInt::new_attr_val(*v),
        Arg::Str(v) => v.replace('\n', " ").into(),
        Arg::IStr(v) => v.replace('\n', " ").into(),
        Arg::Char(v) => v.to_string().into(),
        Arg::Preformatted(v) => v.replace('\n', " ").into(),
        Arg::Format { format: _, args } => {
            // We only support single terminal types here currently
            if args.len() == 1 {
                return arg_to_attr_val(&args[0]);
            } else {
                return None;
            }
        }
        Arg::FormatSlice { elements: _ } | Arg::FormatSequence { args: _ } | Arg::Slice(_) => {
            return None
        }
    })
}

fn extract_literal_key_value_pairs(s: &str) -> BTreeMap<String, AttrVal> {
    let mut pairs = BTreeMap::new();
    let possible_pairs: Vec<&str> = s.split(',').collect();
    for pair in possible_pairs.into_iter() {
        let parts: Vec<&str> = pair.trim().split('=').map(|p| p.trim()).collect();
        if parts.len() != 2
            || parts[0].is_empty()
            || parts[1].is_empty()
            || parts[0].starts_with('.')
        {
            continue;
        }

        let key: &str = parts[0];
        let val_str: &str = parts[1];
        if let Ok(val) = val_str.parse() {
            pairs.insert(key.to_owned(), val);
        }
    }
    pairs
}

#[derive(Debug, Copy, Clone)]
enum Timestamp {
    Micros(u64),
    Millis(u64),
    Seconds(u64),
    Ticks(u64),
}

impl Timestamp {
    fn from_frame(f: &Frame<'_>) -> Option<Self> {
        let fmt = f.timestamp_format()?;

        // TODO: refactor so we don't spam the log every frame when unsupported
        if f.timestamp_args().len() != 1 {
            warn!("Unsupported timestamp format, only a single argument is supported");
            return None;
        }

        let ts = if let Some(ts) = ts_from_arg(&f.timestamp_args()[0]) {
            ts
        } else {
            warn!("Unsupported timestamp format, only u64 compatible types are supported");
            return None;
        };

        let ts_fmt = fmt
            .trim_end_matches('}')
            .rsplit_once(':')
            .map(|(_, rhs)| rhs);

        Some(match ts_fmt {
            Some("us") | Some("tus") => Timestamp::Micros(ts),
            Some("ms") | Some("tms") => Timestamp::Millis(ts),
            Some("ts") => Timestamp::Seconds(ts),
            Some(_) => {
                warn!("Unsupported timestamp format hint, only us, ms, ts, tms, and tus are supported");
                return None;
            }
            None => Timestamp::Ticks(ts),
        })
    }

    fn as_u64(&self) -> u64 {
        use Timestamp::*;
        match self {
            Micros(v) | Millis(v) | Seconds(v) | Ticks(v) => *v,
        }
    }

    fn as_nanoseconds(&self) -> Option<Nanoseconds> {
        use Timestamp::*;
        match self {
            Micros(v) => v.checked_mul(1_000),
            Millis(v) => v.checked_mul(1_000_000),
            Seconds(v) => v.checked_mul(1_000_000_000),
            Ticks(_) => return None,
        }
        .map(Nanoseconds::from)
    }

    fn typ_str(&self) -> &str {
        use Timestamp::*;
        match self {
            Micros(_) => "us",
            Millis(_) => "ms",
            Seconds(_) => "s",
            Ticks(_) => "ticks",
        }
    }
}

fn ts_from_arg(arg: &Arg<'_>) -> Option<u64> {
    Some(match arg {
        Arg::Uxx(v) => u64::try_from(*v).ok()?,
        Arg::Ixx(v) => u64::try_from(*v).ok()?,
        _ => return None,
    })
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub enum DeviantEventKind {
    MutatorAnnounced,
    MutatorRetired,
    MutationCmdCommunicated,
    MutationClearCommunicated,
    MutationTriggered,
    MutationInjected,
}

impl DeviantEventKind {
    fn from_event_name(event_name: &str) -> Option<Self> {
        use DeviantEventKind::*;
        Some(match event_name {
            "modality.mutator.announced" => MutatorAnnounced,
            "modality.mutator.retired" => MutatorRetired,
            "modality.mutation.command_communicated" => MutationCmdCommunicated,
            "modality.mutation.clear_communicated" => MutationClearCommunicated,
            "modality.mutation.triggered" => MutationTriggered,
            "modality.mutation.injected" => MutationInjected,
            _ => return None,
        })
    }
}

fn uuid_to_integer_attr_val(u: &Uuid) -> AttrVal {
    i128::from_le_bytes(*u.as_bytes()).into()
}

#[cfg(test)]
mod test {
    use super::*;
    use defmt_decoder::{Table, TableEntry, Tag};
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    #[test]
    fn simple_literal() {
        let entries = vec![TableEntry::new_without_symbol(
            Tag::Info,
            "Hello, world!".to_owned(),
        )];
        let timestamp = TableEntry::new_without_symbol(Tag::Timestamp, "{=u8:us}".to_owned());
        let table = Table::new_test_table(Some(timestamp), entries);
        let bytes = [
            0, 0, // index
            2, // timestamp
        ];
        let (frame, _) = table.decode(&bytes).unwrap();
        let loc = Location {
            file: PathBuf::from("/foo/src/main.rs"),
            line: 12,
            module: "bar".to_owned(),
        };
        let event_record = EventRecord::from_frame(frame, Some(&loc)).unwrap();
        assert_eq!(event_record.event_name(), Some("Hello, world!"));
        let attrs = event_record
            .attributes
            .into_iter()
            .map(|(k, v)| (k, v))
            .collect::<Vec<_>>();
        dbg!(&attrs);
        assert_eq!(
            attrs,
            vec![
                (
                    "event.internal.defmt.formatted_string".to_owned(),
                    AttrVal::String("Hello, world!".to_owned().into())
                ),
                (
                    "event.internal.defmt.table_index".to_owned(),
                    AttrVal::Integer(0),
                ),
                (
                    "event.internal.defmt.timestamp".to_owned(),
                    AttrVal::Integer(2),
                ),
                (
                    "event.internal.defmt.timestamp.type".to_owned(),
                    AttrVal::String("us".to_owned().into())
                ),
                (
                    "event.level".to_owned(),
                    AttrVal::String("info".to_owned().into())
                ),
                (
                    "event.name".to_owned(),
                    AttrVal::String("Hello, world!".to_owned().into())
                ),
                (
                    "event.source.file".to_owned(),
                    AttrVal::String("/foo/src/main.rs".to_owned().into())
                ),
                ("event.source.line".to_owned(), AttrVal::Integer(12)),
                (
                    "event.source.module".to_owned(),
                    AttrVal::String("bar".to_owned().into())
                ),
                (
                    "event.source.uri".to_owned(),
                    AttrVal::String("file:///foo/src/main.rs:12".to_owned().into())
                ),
                (
                    "event.timestamp".to_owned(),
                    AttrVal::Timestamp(2_000_u64.into()),
                ),
            ]
        );
    }

    #[test]
    fn literal_named_event_with_typed_args() {
        let entries = vec![TableEntry::new_without_symbol(
            Tag::Debug,
            "my_event:: some foo str = {=str}, bar_int={=u8}".to_owned(),
        )];
        let table = Table::new_test_table(None, entries);
        let bytes = [
            0, 0, // index
            5, 0, 0, 0, // length of the string
            b'H', b'e', b'l', b'l', b'o', // string "Hello"
            2,    // u8
        ];
        let (frame, _) = table.decode(&bytes).unwrap();
        let event_record = EventRecord::from_frame(frame, None).unwrap();
        assert_eq!(event_record.event_name(), Some("my_event"));
        let attrs = event_record
            .attributes
            .into_iter()
            .map(|(k, v)| (k, v))
            .collect::<Vec<_>>();
        dbg!(&attrs);
        assert_eq!(
            attrs[0],
            ("event.bar_int".to_owned(), BigInt::new_attr_val(2))
        );
        assert_eq!(
            attrs[7],
            (
                "event.some_foo_str".to_owned(),
                AttrVal::String("Hello".to_owned().into())
            )
        );
        assert_eq!(
            attrs[6],
            (
                "event.name".to_owned(),
                AttrVal::String("my_event".to_owned().into())
            )
        );
    }

    #[test]
    fn literal_attr_values() {
        let entries = vec![TableEntry::new_without_symbol(
            Tag::Info,
            "my_event::k0.k00.k000=1,k1='foo',k2=\"bar\",k3=12.3,k4=true,k5=biz".to_owned(),
        )];
        let table = Table::new_test_table(None, entries);
        let bytes = [
            0, 0, // index
        ];
        let (frame, _) = table.decode(&bytes).unwrap();
        let event_record = EventRecord::from_frame(frame, None).unwrap();
        assert_eq!(event_record.event_name(), Some("my_event"));
        let attrs = event_record
            .attributes
            .into_iter()
            .map(|(k, v)| (k, v))
            .collect::<Vec<_>>();
        dbg!(&attrs);
        assert_eq!(
            attrs[2],
            ("event.k0.k00.k000".to_owned(), AttrVal::Integer(1))
        );
        assert_eq!(attrs[3], ("event.k1".to_owned(), "foo".into()));
        assert_eq!(attrs[4], ("event.k2".to_owned(), "bar".into()));
        assert_eq!(attrs[5], ("event.k3".to_owned(), 12.3_f64.into()));
        assert_eq!(attrs[6], ("event.k4".to_owned(), true.into()));
        assert_eq!(attrs[7], ("event.k5".to_owned(), "biz".into()));
    }

    #[test]
    fn mixed_literal_param_attr_values() {
        let entries = vec![TableEntry::new_without_symbol(
            Tag::Info,
            "FOO::task=blinky_blue,instant={=u64},arg_cnt=0,queue_index={=u8}".to_owned(),
        )];
        let table = Table::new_test_table(None, entries);
        let bytes = [
            0, 0, // index
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // u64
            1,    // u8
        ];
        let (frame, _) = table.decode(&bytes).unwrap();
        let event_record = EventRecord::from_frame(frame, None).unwrap();
        assert_eq!(event_record.event_name(), Some("FOO"));
        let attrs = event_record
            .attributes
            .into_iter()
            .map(|(k, v)| (k, v))
            .collect::<Vec<_>>();
        dbg!(&attrs);
        assert_eq!(attrs[0], ("event.arg_cnt".to_owned(), 0_u8.into()));
        assert_eq!(
            attrs[1],
            (
                "event.instant".to_owned(),
                BigInt::new_attr_val(u64::MAX.into())
            )
        );
        assert_eq!(attrs[8], ("event.queue_index".to_owned(), 1_u8.into()));
        assert_eq!(attrs[9], ("event.task".to_owned(), "blinky_blue".into()));
    }
}
