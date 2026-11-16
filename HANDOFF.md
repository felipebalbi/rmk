# Ordered-input footprint investigation handoff

## Status and scope

This is the experimental branch `experiment/pending-input-buffer`, based on
the pushed PR #1347 tip `f9efbdbb53acb391a691a8da304a1c1e09f41f92`.
Only this handoff is committed beyond that tip. No runtime optimization or
dedicated pending buffer has been applied to this branch yet.

The goal is to reduce the ordered implementation's source complexity and
FLASH/RAM cost without weakening its existing behavioral tests. A separate
pending-input buffer is permitted for this experiment, but the submitted PR
must continue reusing `held_buffer` unless HaoBoGu accepts the alternative.

Important user constraints:

- Focus on the extra code introduced by this PR, not unrelated old-code cleanup.
- Do not change `process_action_osl` or count its cleanup as a PR-specific gain.
- Preserve event ordering, original timing, binding/release ownership, combo
  causality, Morse continuations, runtime configuration behavior and recovery.
- The admission budget remains 16 generations. The proposed 32 refers to
  **bytes per held-key record**, not 32 entries.
- Do not claim that a smaller held record reduces total storage: include the
  dedicated buffer and async task storage in measurements.
- Wrap every commit-message line at 72 columns. Do not create a PR for this
  experimental branch.

The only unfinished source edit was an unrun layout assertion expecting
`size_of::<HeldKey>() == 32`. It was removed before this documentation commit
so the branch does not contain an intentionally failing test. Its intended
RED assertion is preserved below for when representation work starts:

```rust
#[test]
fn held_record_fits_in_32_bytes() {
    assert_eq!(core::mem::size_of::<super::HeldKey>(), 32);
}
```

Put it in `rmk/src/keyboard/held_buffer.rs`'s test module. The present record
is 128 bytes, so this is not expected to pass until that work is implemented.

## Existing PR history

The five commits on the submitted branch, all message lines <=72 columns:

1. `9dac9eb02`: combo timestamp fix.
2. `2c2ade2ab`: simulator preparation and balanced Again-key test.
3. `dacb1bd05`: separate keymap selection from layer-cache updates.
4. `2c416ac69`: separate fork selection from activation.
5. `f9efbdbb5`: ordered implementation, async optimizations, tests and docs.

The history split preserved the final tree exactly. Battery notification
changes were removed from this PR into a separate fix earlier.

## Baseline measurements

Measured on Windows with Rust 1.100.0, target `thumbv7m-none-eabi`, locked
dependencies, release `opt-level = "z"`, fat LTO and one codegen unit.
Source paths were remapped as in CI and compiler warnings denied.

| STM32F1 section | Same-base main | Submitted PR | Delta |
| --- | ---: | ---: | ---: |
| `.text` | 56,428 | 65,412 | +8,984 |
| `.rodata` | 1,096 | 1,104 | +8 |
| `.data` | 32 | 32 | 0 |
| `.bss` | 6,936 | 9,304 | +2,368 |
| FLASH aggregate | 57,792 | 66,784 | +8,992 |

Main is `f7f088e844ff994b6770432f44972dc5ffe0604e`. `.text` includes
instructions, inline literal pools and padding, not exclusively instructions.
Debug sections do not contribute to this FLASH delta. The entire `.bss`
increase is the main Embassy task pool: 4,976 -> 7,344 bytes.

The core commit causes almost all the increase:

| Cumulative revision | FLASH bytes | Increment |
| --- | ---: | ---: |
| main | 57,792 | baseline |
| combo fix | 57,824 | +32 |
| simulator preparation | 57,824 | 0 |
| keymap refactor | 57,836 | +12 |
| fork refactor | 57,860 | +24 |
| ordered implementation | 66,784 | +8,924 |

## Where the linked code resides

Unique function addresses were counted once to avoid alias double-counting.
This accounting sums to the `.text` delta, but is NOT a marginal cost per
feature: LTO changes which existing functions are inlined or outlined.

| Symbol category | Net bytes |
| --- | ---: |
| New ordered scheduling/observation symbols | +7,748 |
| Other named keyboard functions | +4,378 |
| Held-buffer helpers | +1,386 |
| Combined main task poll | -1,396 |
| Removed held-key sort instantiations | -2,936 |
| Other code | -216 |
| Formatting/panic helpers | +8 |
| Memory-copy helper implementations | 0 |
| Alignment/other uncovered `.text` bytes | +12 |
| Total | +8,984 |

Largest new ordered symbols:

- `drain_ordered_keys`: 3,608 bytes, including inlined ordered event execution.
- `fire_ordered_combo`: 808 bytes.
- `observe_ordered_event`: 738 bytes.
- `ordered_resolution`: 712 bytes.
- `observe_pending_morse_inputs`: 652 bytes.

The `memcpy`/`memmove`/`memset` implementations remain 2,490 bytes in both
binaries. Their call sites increase from 56 to 73. Copying adds call/setup and
temporary-management code; the PR did not pull in a second copy library.
Formatting and panic strings are not the explanation for the ~9 KiB.

## Async audit: what is and is not established

The eight new async functions in `ordered.rs` all reach genuine waits:

| Function | Why it can suspend |
| --- | --- |
| `process_ordered_input` | Resolution, combo processing and draining |
| `enqueue_action` | Observation can resolve Morse and emit actions |
| `observe_ordered_event` | Morse timeouts and held-key execution |
| `observe_pending_morse_inputs` | Calls observation/execution |
| `ordered_combo_timeout` | Delayed combo dispatch and observation |
| `fire_ordered_combo` | Observation after synchronous admission work |
| `drain_ordered_keys` | Action execution and live macro/mouse/BLE deadlines |
| `execute_ordered_event` | Event/report queues and synthetic taps |

Pure policy, selection, admission and buffer helpers are synchronous.
`resolve_ordered_owner` is already a synchronous future factory that captures
a small snapshot before awaiting action execution.

The real waits include report-channel capacity in `send_hid_report`, event
publication backpressure in `publish_event_async`, and existing Timer-based
synthetic-tap delays. These suspend cooperatively. No new blocking syscall,
blocking sleep, busy-wait or production `block_on` was found in the ordered
path. `block_on` occurrences inspected in the keyboard module are test code.
This was not a full hardware-driver audit across every feature combination.

However, legitimate waits do not prove this async layering is efficient.
A possible chain is:

```text
process_ordered_input -> process_combo -> trigger_delayed_combo
  -> fire_ordered_combo -> observe_pending_morse_inputs
  -> observe_ordered_event -> fire_held_keys
  -> action execution -> event/report queue
```

The implementation adds an admission scheduler, a separate ordered resolver
and an observation/replay bridge around existing Morse processing. That is
much more machinery than changing one resolution policy. We have NOT proved
that this architecture or its 8,924-byte core cost is necessary, nor that async
state machines alone account for that cost.

A PR-only sizing probe extracted synchronous preparation from the NEW
`fire_ordered_combo` into a normal function followed by an async observer.
It made FLASH **56 bytes larger**, with no RAM reduction. It was discarded
after sizing, not behavior-validated. Do not blindly remove async keywords
or assume synchronous extraction necessarily improves the linked image.

Pre-existing `process_action_osl` and the old decision-list API were inspected
in separate discarded probes. They are explicitly out of the proposed scope
and are NOT included in the useful gains below. Do not modify them as part of
this investigation unless the user changes that direction.

## Two useful PR-only probes, not applied to this branch

Complete replacement function bodies are preserved in the appendices. They
need no new buffer, API, feature flag, capacity reduction or test weakening.

### A. Select the next pending event without fat owned temporaries

Replace the `filter_map/min_by_key` pipeline in `HeldBuffer::next_pending_event`
with a single scan that borrows capture metadata and constructs only the best
candidate. Keep the same three-part causal ordering key and first-minimum tie
behavior. Its function plus selection closure shrink from 618 to 300 bytes;
five memory-helper call sites disappear. Net CB diagnostic FLASH saving:
**316 bytes**.

### B. Resolve the ordered policy in one scan

Replace repeated reconstruction of every physical edge in `ordered_resolution`
with selection of the earliest relevant decision/release. Net CB diagnostic
FLASH saving alone: **160 bytes**.

Both changes together pass the same existing tests, assertions unchanged:

- No default features: **573/573**.
- `rynk,storage`: **663/663**.
- `vial,host_lock,storage,steno`: **636/636**.
- Nightly formatting and Clippy all-targets with `rynk,storage`, `-D warnings`.
- BLE and DFU test configurations compile-check successfully.

Native Windows BLE/DFU execution has known upstream linker failures (crypto
hooks / embedded linker-script symbols). Those runtime rows were NOT claimed
passing. Full Linux CI should be rerun before promoting any probe.

With both probes, the original C8 build still fails, but its final required
FLASH region falls **66,784 -> 66,336 bytes**, reducing overflow from
**1,248 -> 800 bytes**. This recovers **448 bytes of region headroom**.
The initialized load ends at 66,312; linker alignment accounts for the rest.
Do not confuse required-region savings with the CB payload savings above.

These are throwaway feasibility probes, not independently reviewed production
changes or a proof of equivalence for every possible event sequence. Before
adopting B, add differential/property checks against the current resolver for
valid monotonic physical-input traces, exact deadline boundaries, release
precedence, unilateral rules and combo exclusions. Inspect assumptions rather
than using the passing suite as permission to weaken them.

## CI failure and separate target-selection fix

Failing run: https://github.com/rmk-rs/rmk/actions/runs/43903835007

The underlying failure is job 133427341998: `use_config/stm32f1` overflows
FLASH by 1,248 bytes. Job 133431400954 is only the aggregate CI gate.
Old passing run 42270839763 used Rust 1.99.1 and merge tree
`76ce166df2cecb5e702660c582ffba1fe453043e` (PR head `c3719f6f2`).
Its FLASH usage was 65,000 bytes, leaving 536 bytes in a 64 KiB region.
The failing merge tree `a648d8ec077dd213f3b51a44e5e1a459daa7c94c`
has exactly the submitted PR's current tree.

Controlled cross-builds reproduced both CI results exactly. Rust 1.100 is
not the culprit: the old CI tree shrinks to 64,816 bytes under it, while the
new PR overflows even under Rust 1.99.1. The immediate post-rebase version
`fa60cda2e` already overflowed by 1,280 bytes. Later async optimizations helped
slightly on STM32F1; the history split preserved code exactly.

The main footprint increase came with upstream PR #1298, the Embassy migration
(`f219edb2f`, `f46545b2a`). Holding the current PR's six runtime files constant,
upstream immediately before that migration uses 65,356 bytes and fits; after
the migration it requires 66,688 and does not. Current upstream adds another
96 bytes. Main alone still fits. This is a combined footprint issue, not
evidence that #1298 was merged with failing final CI: its final CI passed.

A separate, verified configuration fix was prepared locally:

- Branch: `fix/stm32f1-chip-selection`.
- Commit: `8d812cd2002753d7774ad0e8e964629f45c9e3d8`.
- Based on upstream main `f7f088e84`; one line in
  `examples/use_config/stm32f1/Cargo.toml`: `stm32f103c8` -> `stm32f103cb`.
- This matches its `keyboard.toml`, probe runner and the Rust-based example,
  which already select CB. C8 has 64 KiB FLASH; CB has 128 KiB.
- Both STM32F1 examples build with stable `--release --locked -D warnings`.
- This branch was NOT pushed when prepared. Only the experimental branch is
  being pushed for this handoff. If the commit cannot be fetched on the next
  machine, recreate that one-line change on a new branch from upstream/main.
- Suggested commit title: `fix(examples): align STM32F1 HAL feature with declared chip`.
- Suggested PR description: Align the config-based STM32F1 example with the
  F103CB selected in `keyboard.toml` and the probe runner. Use the matching
  HAL memory map, consistent with the Rust-based example.

That fix does NOT shrink firmware and must remain separate from this work.

## Reproduce / resume on another machine

Start by reading `AGENTS.md` and `rmk/tests/scenarios/README.md`. Preserve this
documentation branch; use another experimental branch or worktree to apply
the appendix functions. There is no dependency on this machine's temp files.
Temporary ELF files and raw logs were not committed to avoid bloating Git.

Use the same compiler/dependencies/profile for comparisons. For a minimal
symbol inspection after building, Rust's `llvm-tools` component provides:

```sh
rustc --version --verbose
rustc --print sysroot
rustc -vV                 # host triple locates lib/rustlib/<host>/bin
llvm-size -A firmware.elf
llvm-nm --demangle --print-size --size-sort --radix=d firmware.elf
llvm-objdump -d --demangle --no-show-raw-insn firmware.elf
```

Invoke the LLVM tools by their full sysroot paths if not on PATH. The analysis
used CB-selected temporary copies to emit the otherwise oversized PR ELF,
with the same selection for main and PR. Explicitly rebuild C8 afterwards to
check its real limit; do not mistake diagnostic linking for a C8 fix.

Validation commands, from `rmk/`, with candidates applied:

```sh
cargo +nightly fmt --manifest-path Cargo.toml -- --check
cargo nextest run --no-default-features --config-file ../.config/nextest.toml --profile ci
cargo nextest run --no-default-features --features rynk,storage --config-file ../.config/nextest.toml --profile ci
cargo nextest run --no-default-features --features vial,host_lock,storage,steno --config-file ../.config/nextest.toml --profile ci
cargo clippy --no-default-features --features rynk,storage --all-targets -- -D warnings
cargo check --tests --no-default-features --features rynk,_ble,split,async_matrix,storage
cargo check --tests --no-default-features --features split,_dfu,dfu_split,storage,async_matrix
```

Build firmware from `examples/use_config/stm32f1/` using
`cargo +1.100.0 build --release --locked`; install that compiler's target and
LLVM tools first if needed. CI additionally sets `RUSTFLAGS="-D warnings"`
and remaps source/target paths to canonical names. Preserve those flags and
paths when comparing small differences. Also measure RP2040 or a BLE target
before drawing cross-target conclusions; earlier async savings differed
significantly between Cortex-M0 and Cortex-M3.

Recommended next structural investigation: centralize edge ordering and
decision/delivery transitions to reduce the scheduler/legacy-Morse bridge.
Do not assume the remaining ~8.5 KiB is unavoidable, but do not claim a
smaller equivalent implementation until demonstrated. A dedicated buffer
only helps FLASH if it eliminates machinery, not merely relocates `Capture`.
No dedicated pending-buffer implementation exists yet.

## Appendix A: tested replacement for next_pending_event

Replace only this method in `rmk/src/keyboard/held_buffer.rs`:

```rust
pub(crate) fn next_pending_event(&self) -> Option<PendingEvent> {
    let mut next = None;
    let mut best = None;
    for key in &self.keys {
        let Some(capture) = key.capture.as_ref() else { continue };
        if key.state == KeyState::WaitingCombo {
            continue;
        }
        let (stamp, pressed) = match capture.phase {
            DeliveryPhase::Queued => (capture.down, true),
            DeliveryPhase::PressDelivered => {
                let Some(up) = capture.up else { continue };
                (up, false)
            }
            DeliveryPhase::ReleaseDelivered | DeliveryPhase::Consumed => continue,
        };
        let delivery_key = (
            if pressed {
                capture.deliver_before.unwrap_or(stamp.order)
            } else {
                stamp.order
            },
            if pressed && capture.deliver_before.is_some() {
                0u8
            } else {
                1
            },
            stamp.order,
        );
        if best.is_none_or(|current| delivery_key < current) {
            best = Some(delivery_key);
            next = Some(PendingEvent {
                generation: capture.down.order,
                event: KeyboardEvent {
                    pressed,
                    source: key.event.source,
                },
                at: stamp.at,
                order: stamp.order,
            });
        }
    }
    next
}
```

## Appendix B: tested replacement for ordered_resolution

Replace only this method in `rmk/src/keyboard/ordered.rs`:

```rust
fn ordered_resolution(&self, owner: HeldKey, now: Instant) -> Option<Action> {
    let KeyAction::TapHold(tap, hold, _) = owner.action else {
        unreachable!()
    };
    let down = owner.capture.unwrap().down;
    let policy = owner.capture.unwrap().policy.unwrap();
    let mode = policy.mode;
    let unilateral = policy.unilateral;
    let hand = match owner.event.source {
        KeyboardEventSource::Key(pos) => self.keymap.hand_at(pos.row as usize, pos.col as usize),
        _ => unreachable!(),
    };
    let mut decision = owner
        .capture
        .unwrap()
        .up
        .filter(|up| up.at < owner.timeout_time)
        .map(|up| (up.order, tap));
    let mut first_release = None;
    for key in &self.held_buffer.keys {
        let Some(capture) = key.capture.as_ref() else { continue };
        if capture.phase == DeliveryPhase::Consumed
            || key.state == KeyState::WaitingCombo
            || !key.event.source.is_physical()
            || capture.down.order <= down.order
        {
            continue;
        }
        let stamp = if mode == MorseMode::HoldOnOtherPress {
            capture.down
        } else {
            let Some(up) = capture.up else { continue };
            up
        };
        if stamp.at >= owner.timeout_time {
            continue;
        }
        let same_hand = unilateral
            && mode != MorseMode::HoldOnOtherPress
            && match key.event.source {
                KeyboardEventSource::Key(pos) => {
                    hand.is_same_side(self.keymap.hand_at(pos.row as usize, pos.col as usize))
                }
                _ => false,
            };
        if mode == MorseMode::Normal {
            if first_release.is_none_or(|(order, _)| stamp.order < order) {
                first_release = Some((stamp.order, same_hand));
            }
        } else if decision.is_none_or(|(order, _)| stamp.order < order) {
            decision = Some((
                stamp.order,
                if mode == MorseMode::PermissiveHold && unilateral && same_hand {
                    tap
                } else {
                    hold
                },
            ));
        }
    }
    if let Some((_, action)) = decision {
        return Some(action);
    }
    (now >= owner.timeout_time).then_some(if unilateral && first_release.is_some_and(|(_, same_hand)| same_hand) {
        tap
    } else {
        hold
    })
}
```
