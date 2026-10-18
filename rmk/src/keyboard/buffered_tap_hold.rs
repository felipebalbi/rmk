//! Identity and execution state for opt-in physical tap-hold capture.

use embassy_time::Instant;
use rmk_types::action::{Action, KeyAction};
use rmk_types::morse::MorseMode;

use crate::config::Hand;
use crate::event::{KeyboardEvent, KeyboardEventPos};
use crate::keyboard::Keyboard;
use crate::keyboard::held_buffer::KeyState;
use crate::keymap::{HOLD_BUFFER_SIZE, KeyMap};

pub(crate) const CAPTURE_EVENTS: usize = 32;
pub(crate) const PRODUCER_BUDGET: usize = HOLD_BUFFER_SIZE + crate::COMBO_MAX_NUM + 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyIdentity {
    pub pos: KeyboardEventPos,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AdmittedKey {
    pub identity: KeyIdentity,
    pub event: KeyboardEvent,
    pub at: Instant,
    pub sequence: u64,
    pub combo_binding: KeyAction,
    pub last_simple_before: Instant,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExecutionBinding {
    pub identity: KeyIdentity,
    pub binding: KeyAction,
    pub action_down: Option<Action>,
    pub buffered_owner: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PositionCapture {
    pub admitted: Option<AdmittedKey>,
    pub executed: Option<ExecutionBinding>,
    pub completed_tap: Option<(KeyAction, Instant)>,
}

impl PositionCapture {
    pub const EMPTY: Self = Self {
        admitted: None,
        executed: None,
        completed_tap: None,
    };
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DeferredAction {
    pub input: AdmittedKey,
    pub binding: KeyAction,
    pub ordinal: u64,
    pub combo_output: bool,
    pub legacy_observed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolution {
    Tap,
    Hold,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingOwner {
    pub input: AdmittedKey,
    pub binding: KeyAction,
    pub tap: Action,
    pub hold: Action,
    pub deadline: Instant,
    pub mode: MorseMode,
    pub hand: Hand,
    pub unilateral: bool,
    pub first_released_hand: Option<Hand>,
}

pub(crate) struct BufferedState {
    pub engaged: bool,
    pub owner: Option<PendingOwner>,
    pub fifo: heapless::Vec<DeferredAction, CAPTURE_EVENTS>,
    pub batch: heapless::Vec<DeferredAction, PRODUCER_BUDGET>,
    pub combos: [PositionCapture; crate::COMBO_MAX_NUM],
    pub sequence: u64,
    pub ordinal: u64,
    pub admission: Option<AdmittedKey>,
    pub logical: Option<AdmittedKey>,
    pub simple_prefix: heapless::Vec<(Instant, u64), { CAPTURE_EVENTS + 1 }>,
    pub legacy_observed: Option<u64>,
    pub legacy_epoch: u64,
    pub fork_prepared: bool,
}

impl BufferedState {
    pub fn new() -> Self {
        Self {
            engaged: false,
            owner: None,
            fifo: heapless::Vec::new(),
            batch: heapless::Vec::new(),
            combos: [PositionCapture::EMPTY; crate::COMBO_MAX_NUM],
            sequence: 0,
            ordinal: 0,
            admission: None,
            logical: None,
            simple_prefix: heapless::Vec::new(),
            legacy_observed: None,
            legacy_epoch: 0,
            fork_prepared: false,
        }
    }
}

impl Keyboard<'_> {
    pub(crate) fn buffered_next_deadline(&self) -> Option<Instant> {
        if !self.buffered.engaged {
            return None;
        }
        if !self.buffered.batch.is_empty() {
            return Some(Instant::now());
        }
        match self.buffered.owner {
            None => (!self.buffered.fifo.is_empty()).then(Instant::now),
            Some(owner) => {
                let deciding = self.buffered.fifo.iter().any(|record| {
                    let input = record.input;
                    if input.at >= owner.deadline {
                        return true;
                    }
                    if !input.event.pressed && input.identity == owner.input.identity {
                        return true;
                    }
                    if !record.combo_output && input.identity.generation > owner.input.identity.generation {
                        return match owner.mode {
                            MorseMode::PermissiveHold => !input.event.pressed,
                            MorseMode::HoldOnOtherPress => input.event.pressed,
                            MorseMode::Normal => false,
                        };
                    }
                    false
                });
                Some(if deciding { Instant::now() } else { owner.deadline })
            }
        }
    }

    pub(crate) fn is_buffered_tap_hold_enabled(keymap: &KeyMap<'_>, action: &KeyAction) -> bool {
        match action {
            KeyAction::TapHold(_, _, idx) => keymap
                .morse_profile(*idx)
                .buffered_tap_hold()
                .or_else(|| keymap.morse_default_profile().buffered_tap_hold())
                .unwrap_or(false),
            _ => false,
        }
    }

    pub(crate) async fn process_buffered_input(&mut self, event: KeyboardEvent, at: Instant) {
        // Finish a previous producer before admitting another raw input.
        while !self.buffered.batch.is_empty() {
            self.drain_buffered_actions(Instant::now()).await;
            self.service_buffered_live_deadlines().await;
            embassy_futures::yield_now().await;
        }
        self.buffered.engaged = true;
        let Some(input) = self
            .keymap
            .capture_admit(event, at, self.buffered.sequence, self.last_press_time)
        else {
            return;
        };
        self.buffered.admission = Some(input);
        if !self.combo_on || self.process_combo(&input.combo_binding, event, at).await {
            self.process_key_action(&input.combo_binding, event, at).await;
        }
        self.keymap.capture_finish_admission(input);
        self.buffered.admission = None;
        self.service_buffered_live_deadlines().await;
        self.drain_buffered_actions(at).await;
    }

    async fn service_buffered_live_deadlines(&mut self) {
        self.buffered.logical = None;
        self.fire_oneshot_timeout().await;
        #[cfg(feature = "_ble")]
        self.fire_user_hold().await;
        self.fire_mouse_repeat().await;
        self.fire_macro().await;
    }

    fn buffered_live_deadline(&self) -> Option<Instant> {
        [
            self.mouse.next_deadline(),
            self.keymap
                .macros(|macros| macros.is_playing())
                .then_some(self.macro_due),
            #[cfg(feature = "_ble")]
            self.user_hold.map(|(at, _)| at),
            (!self.oneshot_is_protected()).then_some(self.osm_deadline).flatten(),
            (!self.oneshot_is_protected()).then_some(self.osl_deadline).flatten(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn submission_input(
        &mut self,
        binding: KeyAction,
        event: KeyboardEvent,
        at: Instant,
    ) -> Option<AdmittedKey> {
        let input = if let KeyboardEventPos::Combo(idx) = event.pos {
            let slot = self.buffered.combos.get_mut(idx as usize)?;
            if event.pressed {
                assert!(slot.admitted.is_none(), "combo admitted twice without release");
                let input = AdmittedKey {
                    identity: KeyIdentity {
                        pos: event.pos,
                        generation: self.buffered.sequence,
                    },
                    event,
                    at,
                    sequence: self.buffered.sequence,
                    combo_binding: binding,
                    last_simple_before: self.last_press_time,
                };
                slot.admitted = Some(input);
                input
            } else {
                let down = slot.admitted.take()?;
                AdmittedKey {
                    event,
                    at,
                    sequence: self.buffered.sequence,
                    ..down
                }
            }
        } else if let Some(current) = self.buffered.admission.filter(|input| input.event == event) {
            current
        } else {
            let down = self.keymap.capture_admitted(event.pos)?;
            AdmittedKey { event, at, ..down }
        };
        Some(input)
    }

    pub(crate) async fn submit_buffered_action(&mut self, binding: KeyAction, event: KeyboardEvent, at: Instant) {
        let combo_output = matches!(event.pos, KeyboardEventPos::Combo(_));
        let Some(input) = self.submission_input(binding, event, at) else {
            return;
        };

        // Only already-held legacy keys observe admission. Execution of this input
        // still belongs to the outer drain, after every combo callback has returned.
        self.buffered.logical = Some(input);
        let (_, mut decisions) = self.make_decisions_for_keys(&binding, event);
        decisions.retain(|(pos, decision)| *pos != event.pos && !matches!(decision, super::HeldKeyDecision::Normal));
        self.fire_held_keys(super::KeyBehaviorDecision::Ignore, decisions).await;
        self.buffered.logical = None;
        self.buffered.ordinal = self
            .buffered
            .ordinal
            .checked_add(1)
            .expect("submission sequence exhausted");
        let record = DeferredAction {
            input,
            binding,
            ordinal: self.buffered.ordinal,
            combo_output,
            legacy_observed: self.buffered.legacy_epoch,
        };
        // One admission dispatches at most HOLD_BUFFER_SIZE waiting keys,
        // COMBO_MAX_NUM releases, one delayed output and the current key.
        assert!(
            self.buffered.batch.push(record).is_ok(),
            "single-admission producer bound exceeded"
        );
    }

    pub(crate) async fn commit_buffered_owner(&mut self, resolution: Resolution, _at: Instant) {
        let Some(owner) = self.buffered.owner.take() else {
            return;
        };
        let action = match resolution {
            Resolution::Tap => owner.tap,
            Resolution::Hold => owner.hold,
        };
        self.buffered.logical = Some(owner.input);
        self.process_key_action_normal(action, owner.input.event).await;
        self.buffered.logical = None;
    }

    pub(crate) async fn drain_buffered_actions(&mut self, now: Instant) {
        let mut steps = 0;
        loop {
            while !self.buffered.batch.is_empty() && !self.buffered.fifo.is_full() {
                let record = self.buffered.batch.remove(0);
                assert!(self.buffered.fifo.push(record).is_ok());
            }
            if let Some(mut owner) = self.buffered.owner {
                // Older ordinary releases and unresolved legacy releases must not
                // wait for a newer owner. Stateful releases retain chord ordering.
                let prompt = self.buffered.fifo.iter().position(|record| {
                    !record.input.event.pressed
                        && record.input.identity.generation < owner.input.identity.generation
                        && self
                            .keymap
                            .capture_execution(record.input.identity)
                            .is_some_and(|executed| match executed.action_down {
                                Some(Action::Key(rmk_types::keycode::KeyCode::Hid(code))) => !code.is_modifier(),
                                None => self
                                    .held_buffer
                                    .find_pos(record.input.event.pos)
                                    .is_some_and(|key| key.action.is_morse()),
                                _ => false,
                            })
                });
                if let Some(index) = prompt {
                    let record = self.buffered.fifo.remove(index);
                    self.execute_buffered_record(record).await;
                    steps += 1;
                } else {
                    let mut decision = None;
                    for record in &self.buffered.fifo {
                        if record.input.at >= owner.deadline {
                            decision = Some((Self::buffered_timeout_resolution(owner), owner.deadline));
                            break;
                        }
                        if record.input.identity == owner.input.identity && !record.input.event.pressed {
                            decision = Some((Resolution::Tap, record.input.at));
                            break;
                        }
                        if record.combo_output || record.input.identity.generation <= owner.input.identity.generation {
                            continue;
                        }
                        if owner.mode == MorseMode::HoldOnOtherPress && record.input.event.pressed {
                            decision = Some((Resolution::Hold, record.input.at));
                            break;
                        }
                        if !record.input.event.pressed {
                            let hand = match record.input.event.pos {
                                KeyboardEventPos::Key(pos) => self.keymap.hand_at(pos.row as usize, pos.col as usize),
                                _ => Hand::Unknown,
                            };
                            if owner.first_released_hand.is_none() {
                                owner.first_released_hand = Some(hand);
                            }
                            if owner.mode == MorseMode::PermissiveHold {
                                let resolution = if owner.unilateral && owner.hand.is_same_side(hand) {
                                    Resolution::Tap
                                } else {
                                    Resolution::Hold
                                };
                                decision = Some((resolution, record.input.at));
                                break;
                            }
                        }
                    }
                    self.buffered.owner = Some(owner);
                    if decision.is_none() && now >= owner.deadline {
                        decision = Some((Self::buffered_timeout_resolution(owner), owner.deadline));
                    }
                    if decision.is_none() && self.buffered.fifo.is_full() && !self.buffered.batch.is_empty() {
                        warn!("Buffered tap-hold capture exhausted; resolving oldest owner as tap");
                        decision = Some((Resolution::Tap, now));
                    }
                    if let Some((resolution, at)) = decision {
                        self.commit_buffered_owner(resolution, at).await;
                        steps += 1;
                    } else {
                        break;
                    }
                }
            } else if let Some(record) = self.buffered.fifo.first().copied() {
                // Historical legacy Morse deadlines merge with replay events,
                // retaining legacy event-first precedence at an exact tie.
                if let Some(key) = self.held_buffer.next_timeout(|key| {
                    key.action.is_morse()
                        && matches!(
                            key.state,
                            KeyState::Pressed(_) | KeyState::Released(_) | KeyState::EarlyFired(_)
                        )
                        && key.timeout_time < record.input.at
                }) {
                    self.buffered.logical = Some(AdmittedKey {
                        at: key.timeout_time,
                        ..record.input
                    });
                    self.handle_morse_timeout(&key).await;
                    self.buffered.logical = None;
                } else {
                    self.buffered.fifo.remove(0);
                    self.execute_buffered_record(record).await;
                }
                steps += 1;
            } else {
                break;
            }
            if self.buffered_live_deadline().is_some_and(|at| at <= Instant::now()) {
                self.service_buffered_live_deadlines().await;
                break;
            }
            if steps >= 8 || self.keyboard_event_subscriber.available() > 0 {
                embassy_futures::yield_now().await;
                break;
            }
        }
        self.buffered.logical = None;
        self.buffered.legacy_observed = None;
        self.buffered.fork_prepared = false;
        if self.buffered.owner.is_none() && self.buffered.fifo.is_empty() && self.buffered.batch.is_empty() {
            self.buffered.simple_prefix.clear();
        }
    }

    fn buffered_timeout_resolution(owner: PendingOwner) -> Resolution {
        if owner.unilateral
            && owner
                .first_released_hand
                .is_some_and(|hand| owner.hand.is_same_side(hand))
        {
            Resolution::Tap
        } else {
            Resolution::Hold
        }
    }

    async fn execute_buffered_record(&mut self, record: DeferredAction) {
        let input = record.input;
        self.buffered.logical = Some(input);
        self.buffered.legacy_observed = Some(record.legacy_observed);
        if !input.event.pressed {
            let executed = if let KeyboardEventPos::Combo(idx) = input.event.pos {
                self.buffered.combos[idx as usize]
                    .executed
                    .filter(|binding| binding.identity == input.identity)
            } else {
                self.keymap.capture_execution(input.identity)
            };
            if let Some(executed) = executed {
                if executed.buffered_owner
                    || (executed.action_down.is_some()
                        && (!executed.binding.is_morse() || self.held_buffer.find_pos(input.event.pos).is_none()))
                {
                    if let Some(action) = executed.action_down {
                        if matches!(executed.binding, KeyAction::TapHold(tap, _, _) if tap == action) {
                            self.keymap.capture_remember_tap(input, executed.binding);
                        }
                        self.process_key_action_normal(action, input.event).await;
                    }
                    if self.try_finish_forks(input.event) {
                        self.send_keyboard_report_with_resolved_modifiers(false).await;
                    }
                } else {
                    self.process_key_action_legacy(&executed.binding, input.event, input.at)
                        .await;
                }
                if let KeyboardEventPos::Combo(idx) = input.event.pos {
                    self.buffered.combos[idx as usize].executed = None;
                } else {
                    self.keymap.capture_finish_execution(input.identity);
                }
            }
        } else {
            let mut binding = if record.combo_output {
                record.binding
            } else {
                self.keymap.capture_binding(input.event.pos)
            };
            let reserved = if let KeyboardEventPos::Combo(idx) = input.event.pos {
                let slot = &mut self.buffered.combos[idx as usize];
                if slot.executed.is_some() {
                    false
                } else {
                    slot.executed = Some(ExecutionBinding {
                        identity: input.identity,
                        binding,
                        action_down: None,
                        buffered_owner: false,
                    });
                    true
                }
            } else {
                self.keymap.capture_commit_press(input, binding)
            };
            if !reserved {
                error!("Buffered execution ownership unavailable; suppressing press");
                self.buffered.logical = None;
                return;
            }
            let physical_owner = matches!(input.event.pos, KeyboardEventPos::Key(_))
                && Self::is_buffered_tap_hold_enabled(self.keymap, &binding);
            if physical_owner {
                let (selected, dropped) = self.try_start_forks(&binding, input.event);
                binding = selected;
                self.buffered.fork_prepared = true;
                if dropped {
                    self.send_keyboard_report_with_resolved_modifiers(true).await;
                }
            }
            let flow = Self::is_flow_tap_enabled(self.keymap, &binding)
                && self
                    .buffered_recent_simple(input)
                    .is_some_and(|time| input.at.saturating_duration_since(time) < self.keymap.morse_prior_idle_time());
            let quick = self
                .keymap
                .capture_completed_tap(input.event.pos)
                .is_some_and(|(last, released)| {
                    last == binding
                        && Self::quick_tap_window(self.keymap, &binding)
                            .is_some_and(|window| input.at.saturating_duration_since(released) <= window)
                });
            if physical_owner
                && Self::is_buffered_tap_hold_enabled(self.keymap, &binding)
                && let KeyAction::TapHold(tap, hold, _) = binding
            {
                self.keymap.capture_set_owner(input.identity);
                if flow || quick {
                    self.process_key_action_normal(tap, input.event).await;
                } else {
                    let hand = match input.event.pos {
                        KeyboardEventPos::Key(pos) => self.keymap.hand_at(pos.row as usize, pos.col as usize),
                        _ => Hand::Unknown,
                    };
                    self.buffered.owner = Some(PendingOwner {
                        input,
                        binding,
                        tap,
                        hold,
                        deadline: input.at + Self::morse_timeout(self.keymap, &binding, true),
                        mode: Self::tap_hold_mode(self.keymap, &binding),
                        hand,
                        unilateral: Self::is_unilateral_tap_enabled(self.keymap, &binding),
                        first_released_hand: None,
                    });
                }
            } else {
                let needs_held_entry = binding.is_morse()
                    || self.make_decisions_for_keys(&binding, input.event).0 == super::KeyBehaviorDecision::Buffer;
                if needs_held_entry
                    && self.held_buffer.keys.is_full()
                    && self.held_buffer.find_pos(input.event.pos).is_none()
                {
                    warn!("Legacy held ownership exhausted; degrading fresh press to balanced tap");
                    if let KeyboardEventPos::Combo(idx) = input.event.pos {
                        self.buffered.combos[idx as usize]
                            .executed
                            .as_mut()
                            .unwrap()
                            .buffered_owner = true;
                    } else {
                        self.keymap.capture_set_owner(input.identity);
                    }
                    let action = if binding.is_morse() {
                        Self::action_from_pattern(self.keymap, &binding, rmk_types::morse::TAP)
                    } else {
                        binding.to_action()
                    };
                    self.process_key_action_normal(action, input.event).await;
                    self.process_key_action_normal(
                        action,
                        KeyboardEvent {
                            pressed: false,
                            ..input.event
                        },
                    )
                    .await;
                } else {
                    self.process_key_action_legacy(&binding, input.event, input.at).await;
                }
            }
        }
        self.buffered.logical = None;
        self.buffered.fork_prepared = false;
    }

    pub(crate) fn buffered_recent_simple(&self, input: AdmittedKey) -> Option<Instant> {
        self.buffered
            .simple_prefix
            .iter()
            .filter(|(at, sequence)| *sequence < input.sequence && *at <= input.at)
            .map(|(at, _)| *at)
            .chain((input.last_simple_before <= input.at).then_some(input.last_simple_before))
            .max()
    }
}
