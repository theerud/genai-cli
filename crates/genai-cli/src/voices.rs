//! Catalog of the 30 prebuilt voices supported by Gemini's TTS models
//! (2.5-flash / 2.5-pro / 3.1-flash). All three share the same voice
//! set so we keep this model-agnostic.
//!
//! Styles come from Google's published docs; gender is community-
//! curated from gemini-tts.com (Google's API surface doesn't expose
//! gender). Update if Google publishes an official mapping that
//! disagrees.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gender {
    Female,
    Male,
}

impl Gender {
    pub fn label(self) -> &'static str {
        match self {
            Gender::Female => "female",
            Gender::Male => "male",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            Gender::Female => "F",
            Gender::Male => "M",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "f" | "female" => Some(Gender::Female),
            "m" | "male" => Some(Gender::Male),
            _ => None,
        }
    }
}

impl fmt::Display for Gender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Voice {
    pub name: &'static str,
    pub gender: Gender,
    pub style: &'static str,
}

pub const CATALOG: &[Voice] = &[
    Voice { name: "Zephyr",        gender: Gender::Female, style: "bright" },
    Voice { name: "Puck",          gender: Gender::Male,   style: "upbeat" },
    Voice { name: "Charon",        gender: Gender::Male,   style: "informative" },
    Voice { name: "Kore",          gender: Gender::Female, style: "firm" },
    Voice { name: "Fenrir",        gender: Gender::Male,   style: "excitable" },
    Voice { name: "Leda",          gender: Gender::Female, style: "youthful" },
    Voice { name: "Orus",          gender: Gender::Male,   style: "firm" },
    Voice { name: "Aoede",         gender: Gender::Female, style: "breezy" },
    Voice { name: "Callirrhoe",    gender: Gender::Female, style: "easy-going" },
    Voice { name: "Autonoe",       gender: Gender::Female, style: "bright" },
    Voice { name: "Enceladus",     gender: Gender::Male,   style: "breathy" },
    Voice { name: "Iapetus",       gender: Gender::Male,   style: "clear" },
    Voice { name: "Umbriel",       gender: Gender::Male,   style: "easy-going" },
    Voice { name: "Algieba",       gender: Gender::Male,   style: "smooth" },
    Voice { name: "Despina",       gender: Gender::Female, style: "smooth" },
    Voice { name: "Erinome",       gender: Gender::Female, style: "clear" },
    Voice { name: "Algenib",       gender: Gender::Male,   style: "gravelly" },
    Voice { name: "Rasalgethi",    gender: Gender::Male,   style: "informative" },
    Voice { name: "Laomedeia",     gender: Gender::Female, style: "upbeat" },
    Voice { name: "Achernar",      gender: Gender::Female, style: "soft" },
    Voice { name: "Alnilam",       gender: Gender::Male,   style: "firm" },
    Voice { name: "Schedar",       gender: Gender::Male,   style: "even" },
    Voice { name: "Gacrux",        gender: Gender::Female, style: "mature" },
    Voice { name: "Pulcherrima",   gender: Gender::Male,   style: "forward" },
    Voice { name: "Achird",        gender: Gender::Male,   style: "friendly" },
    Voice { name: "Zubenelgenubi", gender: Gender::Male,   style: "casual" },
    Voice { name: "Vindemiatrix",  gender: Gender::Female, style: "gentle" },
    Voice { name: "Sadachbia",     gender: Gender::Male,   style: "lively" },
    Voice { name: "Sadaltager",    gender: Gender::Male,   style: "knowledgeable" },
    Voice { name: "Sulafat",       gender: Gender::Female, style: "warm" },
];

pub fn names() -> Vec<&'static str> {
    CATALOG.iter().map(|v| v.name).collect()
}

#[cfg(test)]
pub fn find(name: &str) -> Option<&'static Voice> {
    CATALOG.iter().find(|v| v.name.eq_ignore_ascii_case(name))
}

pub fn filter(
    gender: Option<Gender>,
    style_substring: Option<&str>,
) -> Vec<&'static Voice> {
    let style_lc = style_substring.map(|s| s.to_ascii_lowercase());
    CATALOG
        .iter()
        .filter(|v| gender.map(|g| g == v.gender).unwrap_or(true))
        .filter(|v| {
            style_lc
                .as_deref()
                .map(|s| v.style.to_ascii_lowercase().contains(s))
                .unwrap_or(true)
        })
        .collect()
}

/// One-line summary suitable for embedding in the LLM's tool-call schema
/// description. Example: `Kore (F, firm), Charon (M, informative), ...`.
pub fn descriptor_list() -> String {
    let parts: Vec<String> = CATALOG
        .iter()
        .map(|v| format!("{} ({}, {})", v.name, v.gender.short(), v.style))
        .collect();
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_size_is_30() {
        assert_eq!(CATALOG.len(), 30);
    }

    #[test]
    fn names_are_unique() {
        let n = names();
        let mut sorted = n.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(n.len(), sorted.len(), "duplicate voice name in catalog");
    }

    #[test]
    fn find_is_case_insensitive() {
        assert!(find("kore").is_some());
        assert!(find("KORE").is_some());
        assert!(find("does-not-exist").is_none());
    }

    #[test]
    fn filter_by_gender() {
        let females: Vec<_> = filter(Some(Gender::Female), None);
        assert_eq!(females.len(), 13, "expected 13 female voices");
        assert!(females.iter().all(|v| v.gender == Gender::Female));
    }

    #[test]
    fn filter_by_style_substring() {
        let firm: Vec<_> = filter(None, Some("firm"));
        // Kore, Orus, Alnilam
        assert_eq!(firm.len(), 3);
    }

    #[test]
    fn filter_combines_gender_and_style() {
        let bright_f: Vec<_> = filter(Some(Gender::Female), Some("bright"));
        // Zephyr, Autonoe
        assert_eq!(bright_f.len(), 2);
    }
}
