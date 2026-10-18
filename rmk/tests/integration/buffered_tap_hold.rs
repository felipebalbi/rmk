//! Historical replay cases needing independently configured owner/legacy terms.

use rmk::config::BehaviorConfig;
use rmk::test_support::test_block_on;
use rmk::types::action::{Action, KeyAction};
use rmk::types::keycode::{HidKeyCode, KeyCode};
use rmk::types::modifier::ModifierCombination;
use rmk::types::morse::{HOLD, Morse, MorseMode, MorseProfile, TAP};

use crate::simulator::SimKeyboard;

#[test]
fn delayed_legacy_morse_release_keeps_its_short_tap_duration() {
    test_block_on(async {
        let key = |code| Action::Key(KeyCode::Hid(code));
        let mut behavior = BehaviorConfig::default();
        behavior.morse.default_profile = MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(500), Some(250))
            .with_buffered_tap_hold(Some(true));
        let mut morse = Morse {
            profile: MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(200), Some(250))
                .with_buffered_tap_hold(Some(false)),
            ..Morse::default()
        };
        let _ = morse.put(TAP, key(HidKeyCode::Z));
        let _ = morse.put(HOLD, Action::Modifier(ModifierCombination::RCTRL));
        let _ = morse.put(TAP.followed_by_tap(), key(HidKeyCode::X));
        behavior.morse.morses.push(morse).unwrap();
        let thumb = KeyAction::TapHold(key(HidKeyCode::Tab), Action::LayerOn(1), u8::MAX);
        let mut keyboard = SimKeyboard::builder([[[thumb, KeyAction::Morse(0)]], [[KeyAction::Transparent; 2]]])
            .behavior_config(behavior)
            .build()
            .await;
        keyboard
            .press(0, 0)
            .delay(50)
            .press(0, 1)
            .delay(50)
            .release(0, 1)
            .delay(450)
            .release(0, 0)
            .expect_keys([HidKeyCode::Z])
            .expect_keys([])
            .run()
            .await;
    });
}

#[test]
fn nested_short_owner_uses_already_queued_release() {
    test_block_on(async {
        let key = |code| Action::Key(KeyCode::Hid(code));
        let mut behavior = BehaviorConfig::default();
        behavior.morse.default_profile = MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(500), Some(250))
            .with_buffered_tap_hold(Some(true));
        behavior
            .morse
            .profiles
            .push(MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(200), None))
            .unwrap();
        let outer = KeyAction::TapHold(key(HidKeyCode::Tab), Action::LayerOn(1), u8::MAX);
        let inner = KeyAction::TapHold(key(HidKeyCode::A), Action::Modifier(ModifierCombination::LSHIFT), 0);
        let mut keyboard = SimKeyboard::builder([[[outer, inner]], [[KeyAction::Transparent; 2]]])
            .behavior_config(behavior)
            .build()
            .await;
        keyboard
            .delay(1000)
            .press(0, 0)
            .delay(50)
            .press(0, 1)
            .delay(50)
            .release(0, 1)
            .delay(450)
            .release(0, 0)
            .expect_keys([HidKeyCode::A])
            .expect_keys([])
            .run()
            .await;
    });
}

#[test]
fn capture_overflow_keeps_repeated_taps_and_incoming_release_balanced() {
    test_block_on(async {
        let mut behavior = BehaviorConfig::default();
        behavior.morse.default_profile = MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(5000), Some(250))
            .with_buffered_tap_hold(Some(true));
        let key = |code| Action::Key(KeyCode::Hid(code));
        let thumb = KeyAction::TapHold(key(HidKeyCode::Tab), Action::LayerOn(1), u8::MAX);
        let mut keyboard = SimKeyboard::builder([
            [[
                thumb,
                KeyAction::Single(key(HidKeyCode::B)),
                KeyAction::Single(key(HidKeyCode::Q)),
            ]],
            [[KeyAction::Transparent; 3]],
        ])
        .behavior_config(behavior)
        .build()
        .await;
        keyboard.delay(1000).press(0, 0).press(0, 1);
        for _ in 0..24 {
            keyboard.press(0, 2).release(0, 2);
        }
        keyboard
            .release(0, 0)
            .release(0, 1)
            .expect_keys([HidKeyCode::Tab])
            .expect_keys([HidKeyCode::Tab, HidKeyCode::B]);
        for _ in 0..24 {
            keyboard
                .expect_keys([HidKeyCode::Tab, HidKeyCode::B, HidKeyCode::Q])
                .expect_keys([HidKeyCode::Tab, HidKeyCode::B]);
        }
        keyboard
            .expect_keys([HidKeyCode::B])
            .expect_keys([])
            .tap(0, 2, 20)
            .expect_keys([HidKeyCode::Q])
            .expect_keys([])
            .run()
            .await;
    });
}

#[test]
fn older_legacy_mt_release_is_not_held_behind_a_new_owner() {
    test_block_on(async {
        let mut behavior = BehaviorConfig::default();
        behavior.morse.default_profile = MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(200), Some(250))
            .with_buffered_tap_hold(Some(true));
        behavior
            .morse
            .profiles
            .push(MorseProfile::const_default().with_buffered_tap_hold(Some(false)))
            .unwrap();
        let key = |code| Action::Key(KeyCode::Hid(code));
        let thumb = KeyAction::TapHold(key(HidKeyCode::Tab), Action::LayerOn(1), u8::MAX);
        let legacy = KeyAction::TapHold(key(HidKeyCode::A), Action::Modifier(ModifierCombination::LSHIFT), 0);
        let mut keyboard = SimKeyboard::builder([[[legacy, thumb]], [[KeyAction::Transparent; 2]]])
            .behavior_config(behavior)
            .build()
            .await;
        keyboard
            .delay(1000)
            .press(0, 0)
            .delay(20)
            .press(0, 1)
            .delay(20)
            .release(0, 0)
            .delay(250)
            .release(0, 1)
            .expect_keys([HidKeyCode::A])
            .expect_keys([])
            .run()
            .await;
    });
}

#[test]
fn full_legacy_held_store_cannot_strand_a_flow_tapped_layer() {
    test_block_on(async {
        let mut behavior = BehaviorConfig::default();
        behavior.morse.default_profile = MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(5000), Some(250))
            .with_buffered_tap_hold(Some(false))
            .with_enable_flow_tap(Some(true));
        behavior.morse.prior_idle_time = embassy_time::Duration::from_secs(5);
        // An available opted-in profile engages the pipeline, while these keys
        // intentionally exercise its mixed legacy ownership path.
        behavior
            .morse
            .profiles
            .push(MorseProfile::const_default().with_buffered_tap_hold(Some(true)))
            .unwrap();
        let legacy = KeyAction::TapHold(Action::No, Action::No, u8::MAX);
        let mut base = [[legacy; 18]];
        base[0][16] = KeyAction::TapHold(Action::LayerOn(1), Action::No, u8::MAX);
        base[0][17] = KeyAction::Single(Action::Key(KeyCode::Hid(HidKeyCode::A)));
        let mut upper = [[KeyAction::Transparent; 18]];
        upper[0][17] = KeyAction::Single(Action::Key(KeyCode::Hid(HidKeyCode::B)));
        let mut keyboard = SimKeyboard::builder([base, upper])
            .behavior_config(behavior)
            .build()
            .await;
        for col in 0..16 {
            keyboard.press(0, col);
        }
        keyboard.press(0, 16).release(0, 16);
        for col in 0..16 {
            keyboard.release(0, col);
        }
        keyboard
            .tap(0, 17, 20)
            .expect_keys([HidKeyCode::A])
            .expect_keys([])
            .run()
            .await;
    });
}

#[test]
fn flow_tap_behind_an_owner_uses_the_original_prefix_time() {
    test_block_on(async {
        let mut behavior = BehaviorConfig::default();
        behavior.morse.default_profile = MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(500), Some(250))
            .with_buffered_tap_hold(Some(true))
            .with_enable_flow_tap(Some(false));
        behavior
            .morse
            .profiles
            .push(MorseProfile::new(None, None, Some(200), None).with_enable_flow_tap(Some(true)))
            .unwrap();
        let key = |code| Action::Key(KeyCode::Hid(code));
        let outer = KeyAction::TapHold(key(HidKeyCode::Tab), Action::LayerOn(1), u8::MAX);
        let inner = KeyAction::TapHold(key(HidKeyCode::A), Action::Modifier(ModifierCombination::LSHIFT), 0);
        let mut keyboard = SimKeyboard::builder([[[outer, inner]], [[KeyAction::Transparent; 2]]])
            .behavior_config(behavior)
            .build()
            .await;
        keyboard
            .delay(1000)
            .press(0, 0)
            .delay(50)
            .press(0, 1)
            .delay(50)
            .release(0, 0)
            .delay(400)
            .release(0, 1)
            .expect_keys([HidKeyCode::Tab])
            .expect_keys([HidKeyCode::Tab, HidKeyCode::A])
            .expect_keys([HidKeyCode::A])
            .expect_keys([])
            .run()
            .await;
    });
}

#[test]
fn buffered_quick_tap_respects_the_effective_window() {
    test_block_on(async {
        for (gap, second_modifier) in [(50, false), (150, true)] {
            let mut behavior = BehaviorConfig::default();
            behavior.morse.default_profile =
                MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(200), Some(250))
                    .with_buffered_tap_hold(Some(true))
                    .with_quick_tap_timeout_ms(Some(120));
            let mt = KeyAction::TapHold(
                Action::Key(KeyCode::Hid(HidKeyCode::A)),
                Action::Modifier(ModifierCombination::LSHIFT),
                u8::MAX,
            );
            let mut keyboard = SimKeyboard::builder([[[mt]]]).behavior_config(behavior).build().await;
            keyboard
                .delay(1000)
                .tap(0, 0, 20)
                .delay(gap)
                .press(0, 0)
                .delay(250)
                .release(0, 0)
                .expect_keys([HidKeyCode::A])
                .expect_keys([]);
            if second_modifier {
                keyboard.expect_keys_with_mods(2, []);
            } else {
                keyboard.expect_keys([HidKeyCode::A]);
            }
            keyboard.expect_keys([]).run().await;
        }
    });
}

#[test]
fn replay_created_legacy_owner_observes_later_captured_presses_once() {
    test_block_on(async {
        let mut behavior = BehaviorConfig::default();
        behavior.morse.default_profile = MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(500), Some(250))
            .with_buffered_tap_hold(Some(true))
            .with_enable_flow_tap(Some(false));
        behavior
            .morse
            .profiles
            .push(
                MorseProfile::new(Some(false), Some(MorseMode::HoldOnOtherPress), Some(200), None)
                    .with_buffered_tap_hold(Some(false)),
            )
            .unwrap();
        let outer = KeyAction::TapHold(Action::No, Action::LayerOn(1), u8::MAX);
        let legacy = KeyAction::TapHold(
            Action::Key(KeyCode::Hid(HidKeyCode::A)),
            Action::Modifier(ModifierCombination::LSHIFT),
            0,
        );
        let letter = KeyAction::Single(Action::Key(KeyCode::Hid(HidKeyCode::B)));
        let mut keyboard = SimKeyboard::builder([[[outer, legacy, letter]], [[KeyAction::Transparent; 3]]])
            .behavior_config(behavior)
            .build()
            .await;
        keyboard
            .delay(1000)
            .press(0, 0)
            .delay(50)
            .press(0, 1)
            .delay(30)
            .press(0, 2)
            .delay(20)
            .release(0, 2)
            .delay(50)
            .release(0, 1)
            .delay(30)
            .release(0, 0)
            .expect_keys_with_mods(2, [])
            .expect_keys_with_mods(2, [HidKeyCode::B])
            .expect_keys_with_mods(2, [])
            .expect_keys([])
            .run()
            .await;
    });
}

#[test]
fn buffered_owner_retains_osl_for_the_captured_press() {
    test_block_on(async {
        let mut behavior = BehaviorConfig::default();
        behavior.one_shot.timeout = embassy_time::Duration::from_millis(100);
        behavior.morse.default_profile =
            MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(200), None).with_buffered_tap_hold(Some(true));
        let key = |code| Action::Key(KeyCode::Hid(code));
        let owner = KeyAction::TapHold(key(HidKeyCode::A), Action::No, u8::MAX);
        let mut keyboard = SimKeyboard::builder([
            [[
                KeyAction::Single(Action::OneShotLayer(1)),
                owner,
                KeyAction::Single(key(HidKeyCode::B)),
            ]],
            [[
                KeyAction::Transparent,
                KeyAction::Transparent,
                KeyAction::Single(key(HidKeyCode::C)),
            ]],
        ])
        .behavior_config(behavior)
        .build()
        .await;
        keyboard
            .delay(1000)
            .tap(0, 0, 10)
            .delay(40)
            .press(0, 1)
            .delay(20)
            .press(0, 2)
            .delay(20)
            .release(0, 2)
            .delay(200)
            .release(0, 1)
            .expect_keys([HidKeyCode::C])
            .expect_keys([])
            .run()
            .await;
    });
}

#[test]
fn live_macro_deadline_is_serviced_between_slow_replay_records() {
    use rmk::types::keyboard_macros::MacroOp;
    test_block_on(async {
        let mut behavior = BehaviorConfig::default();
        behavior.morse.default_profile =
            MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(200), None).with_buffered_tap_hold(Some(true));
        behavior.keyboard_macros = &[&[
            MacroOp::Delay(235),
            MacroOp::Tap(Action::Key(KeyCode::Hid(HidKeyCode::B))),
        ]];
        let owner = KeyAction::TapHold(Action::No, Action::LayerOn(1), u8::MAX);
        let mut keyboard = SimKeyboard::builder([
            [[
                KeyAction::Single(Action::TriggerMacro(0)),
                owner,
                KeyAction::Tap(Action::Key(KeyCode::Hid(HidKeyCode::A))),
            ]],
            [[KeyAction::Transparent; 3]],
        ])
        .behavior_config(behavior)
        .build()
        .await;
        keyboard
            .delay(1000)
            .press(0, 0)
            .release(0, 0)
            .delay(20)
            .press(0, 1)
            .delay(20);
        for _ in 0..6 {
            keyboard.press(0, 2).release(0, 2);
        }
        keyboard.delay(180);
        for _ in 0..2 {
            keyboard
                .expect_keys_within([HidKeyCode::A], 15)
                .expect_keys_within([], 15);
        }
        keyboard
            .expect_keys_within([HidKeyCode::B], 2)
            .expect_keys_within([], 15);
        for _ in 0..4 {
            keyboard
                .expect_keys_within([HidKeyCode::A], 15)
                .expect_keys_within([], 15);
        }
        keyboard.release(0, 1).run().await;
    });
}

#[test]
fn a_full_waiting_combo_store_submits_every_failed_member_in_one_batch() {
    use rmk::keyboard::combo::{Combo, ComboConfig};
    test_block_on(async {
        let key = |code| Action::Key(KeyCode::Hid(code));
        let mut behavior = BehaviorConfig::default();
        behavior.morse.default_profile = MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(5000), None)
            .with_buffered_tap_hold(Some(false))
            .with_enable_flow_tap(Some(false));
        behavior
            .morse
            .profiles
            .push(MorseProfile::const_default().with_buffered_tap_hold(Some(true)))
            .unwrap();
        let mut base = [[KeyAction::No; 18]];
        for (col, action) in base[0].iter_mut().take(16).enumerate() {
            *action = KeyAction::TapHold(key(HidKeyCode::B), Action::No, col as u8 + 1);
        }
        for index in 0..8 {
            behavior.combo.combos[index] = Some(Combo::new(ComboConfig::new(
                [
                    base[0][index * 2],
                    base[0][index * 2 + 1],
                    KeyAction::Single(key(HidKeyCode::F1)),
                ],
                KeyAction::No,
                None,
            )));
        }
        base[0][16] = KeyAction::TapHold(Action::No, Action::No, 0);
        base[0][17] = KeyAction::Single(key(HidKeyCode::Q));
        let mut keyboard = SimKeyboard::builder([base]).behavior_config(behavior).build().await;
        keyboard.delay(1000).press(0, 16);
        for col in 0..16 {
            keyboard.press(0, col);
        }
        keyboard.press(0, 17).release(0, 16);
        for col in 0..16 {
            keyboard.release(0, col);
        }
        keyboard.release(0, 17).expect_keys([HidKeyCode::Q]);
        for _ in 0..16 {
            keyboard
                .expect_keys([HidKeyCode::Q, HidKeyCode::B])
                .expect_keys([HidKeyCode::Q]);
        }
        keyboard.expect_keys([]).run().await;
    });
}

#[test]
fn all_combo_release_outputs_survive_a_full_capture_fifo() {
    use rmk::keyboard::combo::{Combo, ComboConfig};
    test_block_on(async {
        let key = |code| Action::Key(KeyCode::Hid(code));
        let mut behavior = BehaviorConfig::default();
        behavior.morse.default_profile = MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(5000), None)
            .with_buffered_tap_hold(Some(true));
        let codes = [HidKeyCode::A, HidKeyCode::B];
        let mut base = [[KeyAction::No; 11]];
        let common = KeyAction::Single(key(HidKeyCode::Z));
        for (index, code) in codes.into_iter().enumerate() {
            base[0][index] = KeyAction::Single(key(code));
            behavior.combo.combos[index] = Some(Combo::new(ComboConfig::new(
                [base[0][index], common],
                KeyAction::Single(Action::Modifier(ModifierCombination::LSHIFT)),
                None,
            )));
        }
        base[0][8] = common;
        base[0][9] = KeyAction::TapHold(Action::No, Action::No, u8::MAX);
        base[0][10] = KeyAction::Single(key(HidKeyCode::Q));
        let mut keyboard = SimKeyboard::builder([base]).behavior_config(behavior).build().await;
        // Partner first, then common: the common bit survives in the other
        // overlapping combo, matching the existing legacy regression fixture.
        keyboard
            .delay(1000)
            .press(0, 9)
            .press(0, 0)
            .press(0, 8)
            .release(0, 0)
            .press(0, 1)
            .release(0, 1);
        for _ in 0..15 {
            keyboard.press(0, 10).release(0, 10);
        }
        keyboard.release(0, 8).release(0, 9);
        for _ in 0..2 {
            keyboard.expect_keys_with_mods(2, []);
        }
        for _ in 0..15 {
            keyboard
                .expect_keys_with_mods(2, [HidKeyCode::Q])
                .expect_keys_with_mods(2, []);
        }
        keyboard.expect_keys_with_mods(2, []);
        keyboard.expect_keys([]).run().await;
    });
}

#[test]
fn repeated_nested_overflow_with_full_legacy_store_remains_balanced() {
    test_block_on(async {
        let key = |code| Action::Key(KeyCode::Hid(code));
        let mut behavior = BehaviorConfig::default();
        behavior.morse.default_profile = MorseProfile::new(Some(false), Some(MorseMode::Normal), Some(5000), None)
            .with_buffered_tap_hold(Some(false))
            .with_enable_flow_tap(Some(true));
        behavior.morse.prior_idle_time = embassy_time::Duration::from_secs(5);
        behavior
            .morse
            .profiles
            .push(
                MorseProfile::const_default()
                    .with_buffered_tap_hold(Some(true))
                    .with_enable_flow_tap(Some(false)),
            )
            .unwrap();
        let mut base = [[KeyAction::TapHold(Action::No, Action::No, u8::MAX); 20]];
        for (index, code) in [HidKeyCode::A, HidKeyCode::S, HidKeyCode::D].into_iter().enumerate() {
            base[0][16 + index] = KeyAction::TapHold(key(code), Action::Modifier(ModifierCombination::RSHIFT), 0);
        }
        base[0][19] = KeyAction::Single(key(HidKeyCode::Q));
        let mut keyboard = SimKeyboard::builder([base]).behavior_config(behavior).build().await;
        keyboard.delay(1000);
        for col in 0..16 {
            keyboard.press(0, col);
        }
        for _ in 0..2 {
            keyboard.press(0, 16).press(0, 17).press(0, 18).expect_no_report(5);
            for _ in 0..20 {
                keyboard.press(0, 19).release(0, 19);
            }
            keyboard
                .release(0, 16)
                .release(0, 17)
                .release(0, 18)
                .expect_keys([HidKeyCode::A])
                .expect_keys([HidKeyCode::A, HidKeyCode::S])
                .expect_keys([HidKeyCode::A, HidKeyCode::S, HidKeyCode::D]);
            for _ in 0..20 {
                keyboard
                    .expect_keys([HidKeyCode::A, HidKeyCode::S, HidKeyCode::D, HidKeyCode::Q])
                    .expect_keys([HidKeyCode::A, HidKeyCode::S, HidKeyCode::D]);
            }
            keyboard
                .expect_keys([HidKeyCode::S, HidKeyCode::D])
                .expect_keys([HidKeyCode::D])
                .expect_keys([]);
        }
        for col in 0..16 {
            keyboard.release(0, col);
        }
        keyboard
            .tap(0, 19, 10)
            .expect_keys([HidKeyCode::Q])
            .expect_keys([])
            .run()
            .await;
    });
}
