//! An ear that hears (epic 10) never leaves ARIA in hybrid.
//!
//! The first evening with Gemma, 2026-11-18: the model loaded and warm, and
//! not one turn reaching it. ARIA started in hybrid, which waits for the name,
//! and the name is looked for in a transcript that no longer exists.

use aria_core::Core;
use aria_core::audio::{AudioMode, Ear, ModeTrigger};

#[test]
fn switching_to_an_ear_that_hears_listens_actively() {
    let core = Core::new();
    assert_eq!(core.audio_mode(), AudioMode::Hybrid, "the resting mode");
    core.set_ear(Ear::Audio);
    assert_eq!(core.audio_mode(), AudioMode::Active);
}

#[test]
fn reopening_the_ear_goes_straight_to_active_listening() {
    let core = Core::new();
    core.set_ear(Ear::Audio);
    core.apply_audio(ModeTrigger::Interface).expect("off");
    assert_eq!(core.audio_mode(), AudioMode::Off);
    core.apply_audio(ModeTrigger::Shortcut).expect("reopened");
    assert_eq!(core.audio_mode(), AudioMode::Active, "not the deaf hybrid");
}

#[test]
fn off_stays_off_and_a_transcribing_ear_keeps_hybrid() {
    let core = Core::new();
    core.apply_audio(ModeTrigger::Interface).expect("off");
    core.set_ear(Ear::Audio);
    assert_eq!(core.audio_mode(), AudioMode::Off, "the user's hand on the ear");

    let core = Core::new();
    core.set_ear(Ear::Transcript);
    assert_eq!(core.audio_mode(), AudioMode::Hybrid);
    core.apply_audio(ModeTrigger::Interface).expect("off");
    core.apply_audio(ModeTrigger::Interface).expect("reopened");
    assert_eq!(core.audio_mode(), AudioMode::Hybrid, "unchanged for Parakeet");
}

/// Named-only, asked for explicitly, is honoured — even if it means deaf.
#[test]
fn an_explicit_named_only_is_not_overruled() {
    let core = Core::new();
    core.set_ear(Ear::Audio);
    core.apply_audio(ModeTrigger::NamedOnly).expect("named only");
    assert_eq!(core.audio_mode(), AudioMode::Hybrid);
}
