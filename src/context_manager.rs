use crate::{Error, EventRecord, PluginConfig, RtosMode};
use modality_api::{AttrVal, BigInt, TimelineId};
use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use tracing::{debug, trace, warn};

#[derive(Debug)]
pub struct ActiveContext {
    // If there are any synthetic preceding events, they will come first in the list
    pub events: Vec<ContextEvent>,
}

#[derive(Debug)]
pub struct ContextEvent {
    pub context: ContextId,
    pub global_ordering: u128,
    pub record: EventRecord,
    // In fully-linearized-causality mode, every event has a nonce.
    // When true, this event contains an interaction from the previous
    // event and the previous event's nonce should be visible in
    // the conventional attribute key.
    pub add_previous_event_nonce: bool,
}

#[derive(Debug)]
pub struct ContextManager {
    cfg: PluginConfig,
    common_timeline_attrs: TimelineAttributes,
    global_ordering: u128,
    // NOTE: event counter doesn't increment for synthetic events
    event_counter: u64,
    last_timestamp: Option<u64>,
    /// Set when the first EventRecord is the start event in RTOS mode
    integration_version: Option<u16>,
    pending_context_switch_interaction: Option<ContextSwitchInteraction>,
    /// Invariant: always contains the root context as the first element
    context_stack: Vec<ContextId>,
    contexts_to_timelines: BTreeMap<ContextId, TimelineMeta>,
}

impl ContextManager {
    const UNKNOWN_CONTEXT: &'static str = "UNKNOWN_CONTEXT";
    const SYNTHETIC_INTERACTION_EVENT: &'static str = "AUXON_CONTEXT_RETURN";
    const DEFAULT_SINGLE_TIMELINE_CONTEXT_NAME: &'static str = "main";

    pub fn new(cfg: PluginConfig, common_timeline_attrs: TimelineAttributes) -> Self {
        debug!(rtos_mode = %cfg.rtos_mode, "Starting context manager");

        Self {
            cfg,
            common_timeline_attrs,
            global_ordering: 0,
            event_counter: 0,
            last_timestamp: None,
            integration_version: None,
            pending_context_switch_interaction: None,
            context_stack: Default::default(),
            contexts_to_timelines: Default::default(),
        }
    }

    pub fn timeline_meta(&self, context_id: ContextId) -> Result<&TimelineMeta, Error> {
        self.contexts_to_timelines
            .get(&context_id)
            .ok_or(Error::ContextManagerInternalState)
    }

    pub fn process_record(&mut self, mut ev: EventRecord) -> Result<ActiveContext, Error> {
        // NOTE: we assuming the transport provides defmt frames in ordering currently
        self.global_ordering = self.global_ordering.saturating_add(1);

        self.event_counter = self.event_counter.saturating_add(1);
        ev.insert_attr(ev_internal_attr_key("event_counter"), self.event_counter);

        match (self.last_timestamp, ev.timestamp_raw()) {
            (Some(last_t), Some(cur_t)) => {
                if cur_t < last_t {
                    warn!("Event record has a timestamp that went backwards, timestamp rollover possible");
                }
                self.last_timestamp = cur_t.into();
            }
            (None, Some(cur_t)) => {
                self.last_timestamp = cur_t.into();
            }
            (Some(last_t), None) => {
                warn!(
                    last_timestamp = last_t,
                    "Current event record doesn't have a timestamp when the previous record did"
                );
            }
            _ => (),
        }

        if self.cfg.rtos_mode == RtosMode::Rtic1 {
            self.process_rtic1(ev)
        } else {
            // Vanilla mode, all events on a single timeline

            // Setup root/default context timeline
            if self.event_counter == 1 {
                let ctx_name = self
                    .cfg
                    .init_task_name
                    .as_deref()
                    .unwrap_or(Self::DEFAULT_SINGLE_TIMELINE_CONTEXT_NAME)
                    .to_owned();
                let ctx_id = self.alloc_context(&ctx_name);
                // Setup initial context stack
                self.context_stack.push(ctx_id);
            }

            let active_ctx_id = self.active_context()?;
            let timeline = self
                .contexts_to_timelines
                .get_mut(&active_ctx_id)
                .ok_or(Error::ContextManagerInternalState)?;
            timeline.increment_nonce();
            ev.add_internal_nonce(timeline.nonce);

            Ok(ActiveContext {
                events: vec![ContextEvent {
                    context: active_ctx_id,
                    global_ordering: self.global_ordering,
                    record: ev,
                    add_previous_event_nonce: false,
                }],
            })
        }
    }

    fn process_rtic1(&mut self, mut ev: EventRecord) -> Result<ActiveContext, Error> {
        let mut events = Vec::new();

        // Look for the start event, disable RTOS mode if anything doesn't match expectations
        if self.event_counter == 1 && self.integration_version.is_none() {
            let mut start_event_valid = true;
            let event_name = ev.event_name();
            let task_name = ev.task_name();
            let version = ev.integration_version();

            if event_name != Some(rtic1::TRACE_START) {
                warn!(
                    expected_event = rtic1::TRACE_START,
                    "Missing start event, disabling RTOS mode"
                );
                start_event_valid = false;
            }
            if task_name.is_none() {
                warn!("Start event is missing the task name parameter, disabling RTOS mode");
                start_event_valid = false;
            }
            if version.is_none() {
                warn!("Start event is missing the version parameter, disabling RTOS mode");
                start_event_valid = false;
            }

            // Setup a fallback context
            if !start_event_valid {
                self.cfg.rtos_mode = RtosMode::None;
                let ctx_id = self.alloc_context(Self::UNKNOWN_CONTEXT);
                self.context_stack.push(ctx_id);

                events.push(ContextEvent {
                    context: ctx_id,
                    global_ordering: self.global_ordering,
                    record: ev,
                    add_previous_event_nonce: false,
                });
                return Ok(ActiveContext { events });
            };
        }

        let task_or_isr_name = ev.task_name().or_else(|| ev.isr_name());
        let (active_ctx_id, pending_context_switch_interaction) = match (
            ev.event_name(),
            task_or_isr_name,
        ) {
            // Context enter
            (Some(rtic1::TASK_ENTER), Some(ctx_name))
            | (Some(rtic1::ISR_ENTER), Some(ctx_name)) => {
                let ctx_id = self.alloc_context(ctx_name);

                let active_ctx_id = self.active_context()?;
                let active_timeline = self
                    .contexts_to_timelines
                    .get_mut(&active_ctx_id)
                    .ok_or(Error::ContextManagerInternalState)?;

                // Check if we need to generate a synthetic interaction event before
                // entering the new context. Happens when there are no events in
                // between an exit and enter contexts and we don't want to elide
                // the parent context since we're in linear causality mode.
                if active_timeline.requires_synthetic_interaction_event {
                    trace!(ctx_id = active_ctx_id, timeline_id = %active_timeline.id, "Synthesizing interaction event");
                    active_timeline.requires_synthetic_interaction_event = false;

                    let mut syn_record = EventRecord::new(Default::default());

                    syn_record.insert_attr(ev_attr_key("name"), Self::SYNTHETIC_INTERACTION_EVENT);
                    syn_record.insert_attr(ev_internal_attr_key("synthetic"), true);
                    active_timeline.increment_nonce();
                    syn_record.add_internal_nonce(active_timeline.nonce);

                    // Give it the same timestamp as this event
                    if let Some(ts) = ev.attributes().get("event.timestamp") {
                        syn_record.insert_attr(ev_attr_key("timestamp"), ts.clone());
                    }

                    // We should always have one in this case
                    let mut add_previous_event_nonce = !self.cfg.disable_interactions;
                    if let Some(pending_interaction) =
                        self.pending_context_switch_interaction.take()
                    {
                        syn_record.add_interaction(
                            !self.cfg.disable_interactions,
                            pending_interaction.1,
                            pending_interaction.2,
                        );
                    } else {
                        warn!("Missing expected pending interaction for synthetic event");
                        add_previous_event_nonce = false;
                    }

                    // Add the preceding synthetic event
                    events.push(ContextEvent {
                        context: active_ctx_id,
                        global_ordering: self.global_ordering,
                        record: syn_record,
                        add_previous_event_nonce,
                    });
                    self.global_ordering = self.global_ordering.saturating_add(1);
                }

                // Push newly active context, return pending interaction for this event
                let interaction = self.push_context(ctx_id)?;
                (ctx_id, Some(interaction))
            }

            // Context exit
            (Some(rtic1::TASK_EXIT), _) | (Some(rtic1::ISR_EXIT), _) => {
                let ctx_id = self.active_context()?;

                // Return pending interaction for this event
                let pending_interaction_for_this_event =
                    self.pending_context_switch_interaction.take();

                // Store the pending interaction for the next event
                self.pending_context_switch_interaction = self.pop_context()?;

                (ctx_id, pending_interaction_for_this_event)
            }

            // Start event
            (Some(rtic1::TRACE_START), Some(ctx_name)) if self.event_counter == 1 => {
                // SAFETY: start event semantics checked above
                let version = ev.integration_version().unwrap();
                debug!(version, task_name = ctx_name, "Found start event");
                self.integration_version = version.into();
                let init_task_name = self
                    .cfg
                    .init_task_name
                    .as_deref()
                    .unwrap_or(ctx_name)
                    .to_owned();
                // Setup initial context stack
                let ctx_id = self.alloc_context(&init_task_name);
                self.context_stack.push(ctx_id);
                (ctx_id, None)
            }

            event => {
                // Unexpected instrumentation and/or corrupt data
                match event.0 {
                    Some(rtic1::TASK_ENTER) | Some(rtic1::ISR_ENTER) => {
                        warn!("Context enter event is missing the task/isr name parameter, disabling RTOS mode");
                        self.cfg.rtos_mode = RtosMode::None;
                        // Transition to the unknown context
                        let ctx_id = self.alloc_context(Self::UNKNOWN_CONTEXT);
                        self.context_stack.push(ctx_id);
                        self.pending_context_switch_interaction = None;
                    }
                    _ => (),
                }

                // Normal event on the active context
                let active_ctx_id = self.active_context()?;
                let active_timeline = self
                    .contexts_to_timelines
                    .get_mut(&active_ctx_id)
                    .ok_or(Error::ContextManagerInternalState)?;

                // Clear synthetic event marker
                active_timeline.requires_synthetic_interaction_event = false;

                // Return any pending interaction for this event
                (
                    active_ctx_id,
                    self.pending_context_switch_interaction.take(),
                )
            }
        };

        let active_timeline = self
            .contexts_to_timelines
            .get_mut(&active_ctx_id)
            .ok_or(Error::ContextManagerInternalState)?;
        active_timeline.increment_nonce();
        ev.add_internal_nonce(active_timeline.nonce);

        let add_previous_event_nonce = if let Some(interaction) = pending_context_switch_interaction
        {
            ev.add_interaction(!self.cfg.disable_interactions, interaction.1, interaction.2);
            !self.cfg.disable_interactions
        } else {
            false
        };

        // Add the current event
        events.push(ContextEvent {
            context: active_ctx_id,
            global_ordering: self.global_ordering,
            record: ev,
            add_previous_event_nonce,
        });

        Ok(ActiveContext { events })
    }

    fn alloc_context(&mut self, ctx_name: &str) -> ContextId {
        let ctx_id = context_id(ctx_name);
        self.contexts_to_timelines.entry(ctx_id).or_insert_with(|| {
            let mut tl_meta = TimelineMeta::new(ctx_name, ctx_id);
            if let Some(v) = self.integration_version {
                tl_meta.insert_attr(TimelineMeta::internal_attr_key("integration_version"), v);
            }
            tl_meta.insert_attr(
                TimelineMeta::internal_attr_key("rtos_mode"),
                self.cfg.rtos_mode.to_string(),
            );
            for (k, v) in self.common_timeline_attrs.iter() {
                tl_meta.insert_attr(k.clone(), v.clone());
            }

            tl_meta
        });

        ctx_id
    }

    fn active_context(&self) -> Result<ContextId, Error> {
        Ok(*self
            .context_stack
            .last()
            .ok_or(Error::ContextManagerInternalState)?)
    }

    /// Returns the interaction source from the previous context to be added
    /// to the newly active context.
    fn push_context(
        &mut self,
        ctx_id: ContextId,
    ) -> Result<(RemoteContextId, RemoteTimelineId, RemoteInteractionNonce), Error> {
        // Get the previous event's interaction source from the currently active context
        let active_ctx_id = self.active_context()?;
        let active_timeline = self
            .contexts_to_timelines
            .get_mut(&active_ctx_id)
            .ok_or(Error::ContextManagerInternalState)?;
        let interaction = active_timeline.interaction_source();

        // Clear the synthetic event flag since we just got a new context
        // to hang the interaction on
        active_timeline.requires_synthetic_interaction_event = false;

        // Set new context as active
        self.context_stack.push(ctx_id);

        trace!(ctx_id, size = self.context_stack.len(), "Push task");

        Ok(interaction)
    }

    /// Returns Ok(None) when we're back on the root init/unknown context.
    /// This can happen when we started mid-stream and we don't know which tasks we're in.
    fn pop_context(
        &mut self,
    ) -> Result<Option<(RemoteContextId, RemoteTimelineId, RemoteInteractionNonce)>, Error> {
        if self.context_stack.len() == 1 {
            // We're back on the init/unknown context
            if self.integration_version.is_some() {
                warn!("The target should never emit a context exit event from the initial task");
            }
            Ok(None)
        } else {
            // Pop the active context off the stack, previous context now active
            let ctx_id = self
                .context_stack
                .pop()
                .ok_or(Error::ContextManagerInternalState)?;

            let timeline = self
                .contexts_to_timelines
                .get_mut(&ctx_id)
                .ok_or(Error::ContextManagerInternalState)?;

            // Clear the synthetic event flag since we just got a new context
            // to hang the interaction on
            timeline.requires_synthetic_interaction_event = false;

            // Get the interaction source from the previously active context
            let pending_interaction = timeline.next_interaction_source();

            // Mark this context as needed a synthetic interaction event, gets
            // cleared if it receives any events before another context switch.
            // This keeps the causality linear.
            let active_ctx_id = self.active_context()?;
            let active_timeline = self
                .contexts_to_timelines
                .get_mut(&active_ctx_id)
                .ok_or(Error::ContextManagerInternalState)?;
            active_timeline.requires_synthetic_interaction_event = true;

            trace!(
                active_ctx_id,
                prev_ctx_id = ctx_id,
                size = self.context_stack.len(),
                "Pop task"
            );
            Ok(Some(pending_interaction))
        }
    }
}

type RemoteTimelineId = TimelineId;
type RemoteInteractionNonce = i64;
type InteractionNonce = i64;
type ContextSwitchInteraction = (RemoteContextId, RemoteTimelineId, RemoteInteractionNonce);

pub type TimelineAttributes = BTreeMap<String, AttrVal>;

#[derive(Debug)]
pub struct TimelineMeta {
    id: TimelineId,
    ctx_id: ContextId,
    attributes: TimelineAttributes,
    /// The nonce recorded on the last event.
    /// Effectively a timeline-local event counter so we can draw arbitrary interactions
    nonce: InteractionNonce,
    requires_synthetic_interaction_event: bool,
}

impl TimelineMeta {
    const ATTR_KEY_PREFIX: &'static str = "timeline.";
    const INTERNAL_ATTR_KEY_PREFIX: &'static str = "timeline.internal.defmt.";

    pub(crate) fn attr_key(k: &str) -> String {
        format!("{}{k}", Self::ATTR_KEY_PREFIX)
    }

    pub(crate) fn internal_attr_key(k: &str) -> String {
        format!("{}{k}", Self::INTERNAL_ATTR_KEY_PREFIX)
    }

    fn new(ctx_name: &str, ctx_id: ContextId) -> Self {
        let id = TimelineId::allocate();
        trace!(ctx_name, ctx_id, timeline_id = %id, "Creating timeline metadata");

        let mut tlm = Self {
            id,
            ctx_id,
            attributes: Default::default(),
            nonce: 0,
            requires_synthetic_interaction_event: false,
        };
        tlm.insert_attr(Self::attr_key("name"), ctx_name);
        tlm.insert_attr(
            TimelineMeta::internal_attr_key("context.id"),
            BigInt::new_attr_val(ctx_id.into()),
        );

        tlm
    }

    fn insert_attr<V: Into<AttrVal>>(&mut self, k: String, v: V) {
        self.attributes.insert(k, v.into());
    }

    fn increment_nonce(&mut self) {
        self.nonce = self.nonce.wrapping_add(1);
    }

    fn interaction_source(&self) -> (ContextId, TimelineId, InteractionNonce) {
        (self.ctx_id, self.id, self.nonce)
    }

    // For context-pop's, we need post-increment nonce semantics, this keeps
    // the event handling logic cleaner by not having special case nonce handling
    fn next_interaction_source(&self) -> (ContextId, TimelineId, InteractionNonce) {
        (self.ctx_id, self.id, self.nonce.wrapping_add(1))
    }

    pub fn id(&self) -> TimelineId {
        self.id
    }

    pub fn attributes(&self) -> &TimelineAttributes {
        &self.attributes
    }
}

/// A task or ISR identifier, currently just a hash of the string task or ISR name
pub type ContextId = u64;
type RemoteContextId = u64;
fn context_id(ctx_name: &str) -> ContextId {
    let mut h = DefaultHasher::new();
    ctx_name.hash(&mut h);
    h.finish()
}

fn ev_attr_key(k: &str) -> String {
    EventRecord::attr_key(k)
}

fn ev_internal_attr_key(k: &str) -> String {
    EventRecord::internal_attr_key(k)
}

mod rtic1 {
    pub const TRACE_START: &str = "AUXON_TRACE_START";
    pub const TASK_ENTER: &str = "AUXON_TASK_ENTER";
    pub const TASK_EXIT: &str = "AUXON_TASK_EXIT";
    pub const ISR_ENTER: &str = "AUXON_INTERRUPT_ENTER";
    pub const ISR_EXIT: &str = "AUXON_INTERRUPT_EXIT";
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::opts::RtosMode;
    use modality_api::BigInt;
    use pretty_assertions::assert_eq;
    use tracing_test::traced_test;

    fn trace_start(ts: u64) -> EventRecord {
        EventRecord::from_iter(vec![
            (EventRecord::attr_key("name"), rtic1::TRACE_START.into()),
            (EventRecord::attr_key("task"), "init".into()),
            (EventRecord::attr_key("version"), 1_u64.into()),
            (
                EventRecord::internal_attr_key("timestamp"),
                BigInt::new_attr_val(ts.into()),
            ),
        ])
    }

    fn isr_enter(ts: u64) -> EventRecord {
        EventRecord::from_iter(vec![
            (EventRecord::attr_key("name"), rtic1::ISR_ENTER.into()),
            (EventRecord::attr_key("isr"), "ISR".into()),
            (
                EventRecord::internal_attr_key("timestamp"),
                BigInt::new_attr_val(ts.into()),
            ),
        ])
    }

    fn isr_exit(ts: u64) -> EventRecord {
        EventRecord::from_iter(vec![
            (EventRecord::attr_key("name"), rtic1::ISR_EXIT.into()),
            (
                EventRecord::internal_attr_key("timestamp"),
                BigInt::new_attr_val(ts.into()),
            ),
        ])
    }

    fn task_enter(ts: u64) -> EventRecord {
        EventRecord::from_iter(vec![
            (EventRecord::attr_key("name"), rtic1::TASK_ENTER.into()),
            (EventRecord::attr_key("task"), "task".into()),
            (
                EventRecord::internal_attr_key("timestamp"),
                BigInt::new_attr_val(ts.into()),
            ),
        ])
    }

    fn task_exit(ts: u64) -> EventRecord {
        EventRecord::from_iter(vec![
            (EventRecord::attr_key("name"), rtic1::TASK_EXIT.into()),
            (
                EventRecord::internal_attr_key("timestamp"),
                BigInt::new_attr_val(ts.into()),
            ),
        ])
    }

    fn event(name: &str, ts: u64) -> EventRecord {
        EventRecord::from_iter(vec![
            (EventRecord::attr_key("name"), name.into()),
            (
                EventRecord::internal_attr_key("timestamp"),
                BigInt::new_attr_val(ts.into()),
            ),
        ])
    }

    fn check_mngr_state(mngr: &mut ContextManager, active_ctx_name: &str, ts_and_ev_cnt: u64) {
        assert_eq!(mngr.active_context().unwrap(), context_id(active_ctx_name));
        assert_eq!(mngr.event_counter, ts_and_ev_cnt);
        assert_eq!(mngr.last_timestamp, Some(ts_and_ev_cnt));
    }

    fn check_ctx_event(
        ctx_ev: &ContextEvent,
        ctx_name: &str,
        global_ordering: u128,
        int_nonce: i64,
        add_previous_event_nonce: bool,
    ) {
        assert_eq!(ctx_ev.context, context_id(ctx_name));
        assert_eq!(ctx_ev.global_ordering, global_ordering);
        assert_eq!(ctx_ev.record.internal_nonce(), Some(int_nonce));
        assert_eq!(ctx_ev.add_previous_event_nonce, add_previous_event_nonce);
    }

    #[traced_test]
    #[test]
    fn rtic1_context_switching() {
        let mut cfg = PluginConfig::default();
        cfg.rtos_mode = RtosMode::Rtic1;
        let mut mngr = ContextManager::new(cfg, Default::default());

        let ctx = mngr.process_record(trace_start(1)).unwrap();
        assert_eq!(mngr.integration_version, Some(1));
        check_mngr_state(&mut mngr, "init", 1);
        assert_eq!(ctx.events.len(), 1);
        check_ctx_event(&ctx.events[0], "init", 1, 1, false);
        assert_eq!(ctx.events[0].record.integration_version(), Some(1));

        let ctx = mngr.process_record(isr_enter(2)).unwrap();
        check_mngr_state(&mut mngr, "ISR", 2);
        assert_eq!(ctx.events.len(), 1);
        check_ctx_event(&ctx.events[0], "ISR", 2, 1, true);

        let ctx = mngr.process_record(task_enter(3)).unwrap();
        check_mngr_state(&mut mngr, "task", 3);
        assert_eq!(ctx.events.len(), 1);
        check_ctx_event(&ctx.events[0], "task", 3, 1, true);

        let ctx = mngr.process_record(event("foo", 4)).unwrap();
        check_mngr_state(&mut mngr, "task", 4);
        assert_eq!(ctx.events.len(), 1);
        check_ctx_event(&ctx.events[0], "task", 4, 2, false);

        let ctx = mngr.process_record(task_exit(5)).unwrap();
        check_mngr_state(&mut mngr, "ISR", 5); // Pop'd back onto the ISR context
        assert_eq!(ctx.events.len(), 1);
        check_ctx_event(&ctx.events[0], "task", 5, 3, false);

        let ctx = mngr.process_record(isr_exit(6)).unwrap();
        check_mngr_state(&mut mngr, "init", 6); // Pop'd back onto the init context
        assert_eq!(ctx.events.len(), 1);
        check_ctx_event(&ctx.events[0], "ISR", 6, 2, true);

        let ctx = mngr.process_record(isr_enter(7)).unwrap();
        check_mngr_state(&mut mngr, "ISR", 7);
        assert_eq!(ctx.events.len(), 2);
        // Expect a synthetic event
        check_ctx_event(&ctx.events[0], "init", 7, 2, true);
        check_ctx_event(&ctx.events[1], "ISR", 8, 3, true);

        let ctx = mngr.process_record(task_enter(8)).unwrap();
        check_mngr_state(&mut mngr, "task", 8);
        assert_eq!(ctx.events.len(), 1);
        // Synthetic event bumped global_ordering to 9
        check_ctx_event(&ctx.events[0], "task", 9, 4, true);
    }
}
