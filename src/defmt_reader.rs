use crate::{
    Client, ContextEvent, ContextManager, DefmtConfig, Error, EventRecord, Interruptor,
    TimelineAttributes, TimelineMeta,
};
use auxon_sdk::ingest_client::IngestClient;
use defmt_decoder::{DecodeError, Table};
use std::collections::{BTreeMap, BTreeSet};
use std::{fs, io::Read, time::Duration};
use tracing::{debug, warn};
use uuid::Uuid;

pub async fn run<R: Read + Send>(
    mut r: R,
    cfg: DefmtConfig,
    intr: Interruptor,
) -> Result<(), Error> {
    let elf_file = cfg
        .plugin
        .elf_file
        .as_ref()
        .ok_or(Error::MissingElfFile)?
        .clone();
    debug!(elf_file = %elf_file.display(), "Reading ELF file");
    let elf_contents = fs::read(&elf_file).map_err(|e| Error::ElfFileRead(elf_file, e))?;

    debug!("Reading defmt table");
    let table = Table::parse(&elf_contents)
        .map_err(Error::DefmtTable)?
        .ok_or(Error::MissingDefmtSection)?;

    let location_info = {
        // This is essentially what probe-rs reports to the user
        let locs = table
            .get_locations(&elf_contents)
            .map_err(Error::DefmtLocation)?;
        if !table.is_empty() && locs.is_empty() {
            warn!("Insufficient DWARF info; compile your program with `debug = 2` to enable location info.");
            None
        } else if table.indices().all(|idx| locs.contains_key(&(idx as u64))) {
            Some(locs)
        } else {
            warn!("Location info is incomplete; it will be omitted when constructing event attributes.");
            None
        }
    };

    let mut common_timeline_attrs = BTreeMap::new();
    for kv in cfg
        .ingest
        .timeline_attributes
        .additional_timeline_attributes
        .iter()
    {
        common_timeline_attrs.insert(kv.0.to_string(), kv.1.clone());
    }
    let run_id = if let Some(id) = &cfg.plugin.run_id {
        if let Ok(int) = id.parse::<i64>() {
            int.into()
        } else {
            id.into()
        }
    } else {
        Uuid::new_v4().to_string().into()
    };
    common_timeline_attrs.insert(TimelineMeta::attr_key("run_id"), run_id);
    common_timeline_attrs.insert(
        TimelineMeta::internal_attr_key("table.encoding"),
        format!("{:?}", table.encoding()).into(),
    );
    let clock_id = cfg
        .plugin
        .clock_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    common_timeline_attrs.insert(TimelineMeta::attr_key("clock_id"), clock_id.into());
    common_timeline_attrs.insert(TimelineMeta::attr_key("clock_style"), "relative".into());
    if let Some(clock_rate) = cfg.plugin.clock_rate.as_ref() {
        common_timeline_attrs.insert(
            TimelineMeta::attr_key("clock_rate"),
            format!("{}/{}", clock_rate.numerator(), clock_rate.denominator()).into(),
        );
        common_timeline_attrs.insert(
            TimelineMeta::attr_key("clock_rate.numerator"),
            clock_rate.numerator().into(),
        );
        common_timeline_attrs.insert(
            TimelineMeta::attr_key("clock_rate.denominator"),
            clock_rate.denominator().into(),
        );
    }
    for kv in cfg
        .ingest
        .timeline_attributes
        .override_timeline_attributes
        .iter()
    {
        common_timeline_attrs.insert(kv.0.to_string(), kv.1.clone());
    }

    let client = IngestClient::connect_with_timeout(
        &cfg.protocol_parent_url()?,
        cfg.ingest.allow_insecure_tls,
        cfg.plugin
            .client_timeout
            .map(|t| t.0.into())
            .unwrap_or_else(|| Duration::from_secs(1)),
    )
    .await?
    .authenticate(cfg.resolve_auth()?.into())
    .await?;
    let mut client = Client::new(client);

    let mut ctx_mngr = ContextManager::new(cfg.plugin.clone(), common_timeline_attrs);
    let mut observed_timelines = BTreeSet::new();
    let mut buffered_event: Option<ContextEvent> = None;

    let mut decoder = table.new_stream_decoder();
    let mut decoder_buffer = vec![0_u8; 1024];

    debug!("Starting read loop");

    let mut maybe_read_result: Option<Result<(), Error>> = None;
    while !intr.is_set() {
        let bytes_read = match r.read(&mut decoder_buffer) {
            Ok(b) => b,
            Err(e) => {
                // Store the result so we can pass it along after flushing buffered events
                maybe_read_result = Some(Err(e.into()));
                break;
            }
        };
        if bytes_read == 0 {
            // EOF
            break;
        }

        decoder.received(&decoder_buffer[..bytes_read]);
        'read_loop: loop {
            let frame = match decoder.decode() {
                Ok(f) => f,
                Err(e) => match e {
                    DecodeError::UnexpectedEof => {
                        // Need more data
                        break 'read_loop;
                    }
                    DecodeError::Malformed => {
                        warn!("Malformed defmt frame");
                        continue;
                    }
                },
            };
            debug!(msg = %frame.display(false), "Received defmt frame");

            // SAFETY: all of the indices in the table exist in the locations map
            let loc: Option<_> = location_info.as_ref().map(|locs| &locs[&frame.index()]);

            let event_record = EventRecord::from_frame(frame, loc)?;

            let ctx = ctx_mngr.process_record(event_record)?;

            for ev in ctx.events.into_iter() {
                // Maintain a 1-element buffer so we can ensure the interaction nonce attr key
                // is present on the previous event when we encounter a context switch
                // on the current event
                match buffered_event.take() {
                    Some(mut prev_event) => {
                        if ev.add_previous_event_nonce {
                            prev_event.record.promote_internal_nonce();
                        }

                        // Buffer the current event
                        buffered_event = Some(ev);

                        // Send the previous event
                        let timeline = ctx_mngr.timeline_meta(prev_event.context)?;
                        let mut new_timeline_attrs: Option<&TimelineAttributes> = None;
                        if observed_timelines.insert(timeline.id()) {
                            new_timeline_attrs = Some(timeline.attributes());
                        }

                        client
                            .switch_timeline(timeline.id(), new_timeline_attrs)
                            .await?;

                        client
                            .send_event(prev_event.global_ordering, prev_event.record.attributes())
                            .await?;
                    }

                    // First iter of the loop
                    None => {
                        buffered_event = Some(ev);
                    }
                }
            }
        }
    }

    // Flush the last event
    if let Some(last_event) = buffered_event.take() {
        debug!("Flushing buffered events");
        let timeline = ctx_mngr.timeline_meta(last_event.context)?;
        let mut new_timeline_attrs: Option<&TimelineAttributes> = None;
        if observed_timelines.insert(timeline.id()) {
            new_timeline_attrs = Some(timeline.attributes());
        }

        client
            .switch_timeline(timeline.id(), new_timeline_attrs)
            .await?;

        client
            .send_event(last_event.global_ordering, last_event.record.attributes())
            .await?;
    }

    client.inner.flush().await?;

    if let Ok(status) = client.inner.status().await {
        debug!(
            events_received = status.events_received,
            events_written = status.events_written,
            events_pending = status.events_pending,
            "Ingest status"
        );
    }

    if let Some(res) = maybe_read_result {
        res
    } else {
        Ok(())
    }
}
