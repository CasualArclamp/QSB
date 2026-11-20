//! Ready-made channels, from none at all to very rough.

use crate::channel::Path;

pub struct Preset {
    pub name: &'static str,
    pub note: &'static str,
    pub paths: Vec<Path>,
}

/// Two equal, independently fading paths: the form of every ITU-R F.1487 Annex 3
/// test channel (delay between the paths in ms, Doppler spread of each in Hz).
fn itu(name: &'static str, delay_ms: f32, spread_hz: f32) -> Preset {
    Preset {
        name,
        note: "ITU-R F.1487 Annex 3: two equal fading paths",
        paths: vec![
            Path::new(0.0, 0.0, 0.0, spread_hz),
            Path::new(delay_ms, 0.0, 0.0, spread_hz),
        ],
    }
}

pub fn all() -> Vec<Preset> {
    let p = Path::new;
    vec![
        Preset {
            name: "Clean (one path)",
            note: "No multipath",
            paths: vec![p(0.0, 0.0, 0.0, 0.0)],
        },
        Preset {
            name: "Light slow fading",
            note: "A weak fading second path: the level rises and falls gently",
            paths: vec![p(0.0, 0.0, 0.0, 0.0), p(0.3, -8.0, 0.0, 0.08)],
        },
        Preset {
            name: "Slow selective fading",
            note: "A weaker second path: shallow notches drifting slowly",
            paths: vec![p(0.0, 0.0, 0.0, 0.05), p(1.0, -4.0, 0.06, 0.05)],
        },
        Preset {
            name: "Sweeping notches",
            note: "Steady paths with different Doppler shifts: diagonal stripes",
            paths: vec![
                p(0.0, 0.0, 0.0, 0.0),
                p(0.7, -1.0, 0.15, 0.03),
                p(1.9, -5.0, -0.25, 0.08),
            ],
        },
        Preset {
            name: "Intense multipath",
            note: "Four strong paths over 4 ms, each shifted and fading",
            paths: vec![
                p(0.0, 0.0, 0.0, 0.3),
                p(0.8, -1.0, 0.4, 0.5),
                p(2.1, -2.0, -0.6, 0.8),
                p(4.0, -4.0, 1.0, 1.5),
            ],
        },
        // The delay and spread pairs below are the Annex 3 "representative channel
        // parameter combinations" as they are usually quoted (the low-latitude
        // moderate pair checked against MathWorks' iturHFLM description). Check them
        // against the Recommendation before using these for modem figures.
        itu("ITU mid-latitude quiet", 0.5, 0.1),
        itu("ITU mid-latitude moderate", 1.0, 0.5),
        itu("ITU mid-latitude disturbed", 2.0, 1.0),
        itu("ITU mid-latitude disturbed NVIS", 7.0, 1.0),
        itu("ITU low-latitude quiet", 0.5, 0.5),
        itu("ITU low-latitude moderate", 2.0, 1.5),
        itu("ITU low-latitude disturbed", 6.0, 10.0),
        itu("ITU high-latitude quiet", 1.0, 0.5),
        itu("ITU high-latitude moderate", 3.0, 10.0),
        itu("ITU high-latitude disturbed", 7.0, 30.0),
        Preset {
            name: "Auroral flutter",
            note: "One path with a 25 Hz spread: fast flutter, no echo",
            paths: vec![p(0.0, 0.0, 0.0, 25.0)],
        },
    ]
}

/// Find a preset by a part of its name, ignoring case.
pub fn find(name: &str) -> Option<Preset> {
    let want = name.to_lowercase();
    all().into_iter().find(|p| p.name.to_lowercase().contains(&want))
}
